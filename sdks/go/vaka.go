// Package vaka is the Go SDK for Vaka (https://github.com/ErikHellman/vaka):
// errors and panics, breadcrumbs, release health, analytics events and feature
// flags, on top of the standard OpenTelemetry SDK for traces and logs.
//
//	client, err := vaka.Init(vaka.Config{
//		Key:      "vk_...",
//		Endpoint: "https://vaka.example.com",
//		Release:  "1.4.0",
//	})
//	if err != nil {
//		log.Fatal(err)
//	}
//	defer client.Shutdown(context.Background())
//	defer vaka.Recover() // report a panic in main, then re-panic
//
//	if err := run(ctx); err != nil {
//		vaka.CaptureError(ctx, err)
//	}
//
// Init installs OpenTelemetry tracer and logger providers that export OTLP/HTTP
// to the same Vaka server, so spans from any OpenTelemetry instrumentation
// (otelhttp, otelgrpc, database drivers) show up in Vaka's performance views,
// and errors captured with a context carry that context's trace id.
package vaka

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"runtime/debug"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	sdktrace "go.opentelemetry.io/otel/sdk/trace"
)

// Version is the SDK version, sent as `sdk.version`.
const Version = "0.4.0"

const sdkName = "vaka-go"

// Config configures the SDK. Zero values take the defaults described per field.
type Config struct {
	// Key is the project's ingest key (vk_...). Default $VAKA_KEY. Empty disables the SDK.
	Key string
	// Endpoint is the Vaka server URL. Default $VAKA_ENDPOINT.
	Endpoint string
	// Release is the version of the service as users see it. Default $VAKA_RELEASE, then
	// the main module version from the build info, then the VCS revision.
	Release string
	// Environment, default $VAKA_ENVIRONMENT or "production".
	Environment string
	// ServiceName is the OpenTelemetry service.name. Default $OTEL_SERVICE_NAME, then the
	// executable name.
	ServiceName string

	// DisableTracing skips installing the global OpenTelemetry TracerProvider.
	DisableTracing bool
	// DisableLogs skips installing the global OpenTelemetry LoggerProvider.
	DisableLogs bool
	// DisableSessions turns off the per-process release-health session.
	DisableSessions bool
	// TracesSampler samples spans. Default: parent-based, always on.
	TracesSampler sdktrace.Sampler

	// InAppInclude lists package path prefixes that count as your code. Default: the main
	// module path from the build info; without one, every non-standard-library package
	// except vendored and module-cache code.
	InAppInclude []string
	// FlushInterval is how often queued items are sent. Default 5 s.
	FlushInterval time.Duration
	// HTTPClient is used for ingest and flag requests. Default: a client with a 10 s timeout.
	HTTPClient *http.Client
	// BeforeSend may change or drop (return nil) any item before it is queued.
	BeforeSend func(item map[string]any) map[string]any
	// Debug logs SDK activity to stderr.
	Debug bool
}

// Client sends data to Vaka. Create one with Init; the package-level functions use the
// client Init made last. All methods are safe on a nil or disabled client.
type Client struct {
	cfg       Config
	transport *transport
	scope     *Scope
	inApp     []string
	session   *session
	otel      *otelProviders
	disabled  atomic.Bool
	closeOnce sync.Once
}

var current atomic.Pointer[Client]

// CurrentClient returns the client Init created last, or nil.
func CurrentClient() *Client { return current.Load() }

// Init configures the SDK and starts the session. Call it once at program start and
// defer Shutdown. With an empty Key or Endpoint the returned client is a no-op.
func Init(cfg Config) (*Client, error) {
	cfg = withDefaults(cfg)
	c := &Client{cfg: cfg, scope: NewScope()}
	if cfg.Key == "" || cfg.Endpoint == "" {
		c.logf("Key and Endpoint are empty; SDK disabled")
		c.disabled.Store(true)
		current.Store(c)
		return c, nil
	}
	if !strings.HasPrefix(cfg.Endpoint, "http://") && !strings.HasPrefix(cfg.Endpoint, "https://") {
		return nil, fmt.Errorf("vaka: Endpoint must start with http:// or https://, got %q", cfg.Endpoint)
	}
	c.inApp = cfg.InAppInclude
	if len(c.inApp) == 0 {
		if bi, ok := debug.ReadBuildInfo(); ok && bi.Main.Path != "" && bi.Main.Path != "command-line-arguments" {
			c.inApp = []string{bi.Main.Path}
		}
	}
	c.transport = newTransport(cfg)
	otelp, err := setupOTel(cfg)
	if err != nil {
		c.transport.close(context.Background())
		return nil, err
	}
	c.otel = otelp
	current.Store(c)
	if !cfg.DisableSessions {
		c.startSession()
	}
	return c, nil
}

func withDefaults(cfg Config) Config {
	env := func(v *string, name string) {
		if *v == "" {
			*v = os.Getenv(name)
		}
	}
	env(&cfg.Key, "VAKA_KEY")
	env(&cfg.Endpoint, "VAKA_ENDPOINT")
	env(&cfg.Release, "VAKA_RELEASE")
	env(&cfg.Environment, "VAKA_ENVIRONMENT")
	env(&cfg.ServiceName, "OTEL_SERVICE_NAME")
	cfg.Endpoint = strings.TrimRight(cfg.Endpoint, "/")
	if cfg.Release == "" {
		cfg.Release = buildRelease()
	}
	if cfg.Environment == "" {
		cfg.Environment = "production"
	}
	if cfg.ServiceName == "" {
		if exe, err := os.Executable(); err == nil {
			parts := strings.Split(strings.ReplaceAll(exe, "\\", "/"), "/")
			cfg.ServiceName = strings.TrimSuffix(parts[len(parts)-1], ".exe")
		}
	}
	if cfg.FlushInterval <= 0 {
		cfg.FlushInterval = 5 * time.Second
	}
	if cfg.HTTPClient == nil {
		cfg.HTTPClient = &http.Client{Timeout: 10 * time.Second}
	}
	return cfg
}

// buildRelease derives a release from the build info: the module version for
// `go install module@version` builds, otherwise the first 12 characters of the VCS revision.
func buildRelease() string {
	bi, ok := debug.ReadBuildInfo()
	if !ok {
		return ""
	}
	if v := bi.Main.Version; v != "" && v != "(devel)" && !strings.Contains(v, "-0.") {
		return strings.TrimPrefix(v, "v")
	}
	for _, s := range bi.Settings {
		if s.Key == "vcs.revision" && s.Value != "" {
			if len(s.Value) > 12 {
				return s.Value[:12]
			}
			return s.Value
		}
	}
	return ""
}

func (c *Client) enabled() bool { return c != nil && !c.disabled.Load() }

func (c *Client) logf(format string, args ...any) {
	if c != nil && c.cfg.Debug {
		fmt.Fprintf(os.Stderr, "[vaka] "+format+"\n", args...)
	}
}

// Release returns the configured release.
func (c *Client) Release() string {
	if c == nil {
		return ""
	}
	return c.cfg.Release
}

// Scope returns the client's global scope (tags, user, breadcrumbs and flags shared by
// everything the client sends).
func (c *Client) Scope() *Scope {
	if c == nil {
		return NewScope()
	}
	return c.scope
}

// Flush sends everything queued, including OpenTelemetry spans and logs, and waits until
// it is sent or ctx is done.
func (c *Client) Flush(ctx context.Context) error {
	if !c.enabled() {
		return nil
	}
	err := c.transport.flush(ctx)
	if c.otel != nil {
		if e := c.otel.flush(ctx); err == nil {
			err = e
		}
	}
	return err
}

// Shutdown ends the session ("exited", or "errored" after handled errors), flushes and
// stops the SDK, including the OpenTelemetry providers it installed. Later calls are no-ops.
func (c *Client) Shutdown(ctx context.Context) error {
	if !c.enabled() {
		return nil
	}
	var err error
	c.closeOnce.Do(func() {
		c.endSession("exited")
		if c.otel != nil {
			err = c.otel.shutdown(ctx)
		}
		if e := c.transport.close(ctx); err == nil {
			err = e
		}
		c.disabled.Store(true)
	})
	return err
}

func (c *Client) enqueue(item map[string]any) {
	if !c.enabled() {
		return
	}
	if c.cfg.BeforeSend != nil {
		if item = c.cfg.BeforeSend(item); item == nil {
			return
		}
	}
	c.transport.enqueue(item)
}

// Flush flushes the current client. See Client.Flush.
func Flush(ctx context.Context) error { return CurrentClient().Flush(ctx) }

// Shutdown shuts the current client down. See Client.Shutdown.
func Shutdown(ctx context.Context) error { return CurrentClient().Shutdown(ctx) }
