# Install Vaka in a Go service

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.

## 1. Add the module

```sh
go get github.com/ErikHellman/vaka/sdks/go@latest
```

Requires Go 1.24 or newer. The package name is `vaka`; import it as
`vaka "github.com/ErikHellman/vaka/sdks/go"`.

## 2. Initialise at the top of `main`

```go
import vaka "github.com/ErikHellman/vaka/sdks/go"

func main() {
	client, err := vaka.Init(vaka.Config{
		Key:      "{{KEY}}",
		Endpoint: "{{ENDPOINT}}",
		Release:  version, // e.g. set with -ldflags "-X main.version=1.4.0"
	})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Shutdown(context.Background())
	defer vaka.Recover()
	// ... the rest of the program
}
```

`Key`, `Endpoint`, `Release` and `Environment` fall back to `VAKA_KEY`, `VAKA_ENDPOINT`,
`VAKA_RELEASE` and `VAKA_ENVIRONMENT`, so they can come from the environment instead. Without
`Release` the SDK uses the module version or VCS revision from the build info. With an empty key
the SDK is a no-op.

`Init` installs the global OpenTelemetry TracerProvider, LoggerProvider and W3C propagator. If the
service already configures OpenTelemetry, set `DisableTracing: true` and `DisableLogs: true` and
point the existing exporters at Vaka as in [opentelemetry]({{ENDPOINT}}/docs/opentelemetry.md).

## 3. Wire up HTTP and logging

```go
mux := http.NewServeMux()
mux.HandleFunc("GET /carts/{id}", getCart)
log.Fatal(http.ListenAndServe(":8080", vaka.Middleware(mux)))

slog.SetDefault(slog.New(vaka.SlogHandler(slog.NewTextHandler(os.Stderr, nil))))

httpClient := &http.Client{Transport: vaka.Transport(nil)} // outgoing calls join the trace
```

If the service uses `otelhttp` or a framework that already creates server spans, use
`vaka.MiddlewareWithOptions(h, vaka.MiddlewareOptions{DisableSpans: true})`.

Log with the `*Context` methods (`slog.ErrorContext(ctx, ...)`) so records join the request.

## 4. Report errors and events

```go
if err := importCatalog(ctx); err != nil {
	vaka.CaptureError(ctx, err)          // includes the Unwrap chain and the trace id
}
vaka.AddBreadcrumb(ctx, "db", "connected", vaka.LevelInfo)
vaka.SetTag(ctx, "region", "eu-north-1")
vaka.Track(ctx, "job_finished", map[string]any{"kind": "import"})
vaka.SetUser(ctx, "user-123")            // opt-in only

go func() {
	defer vaka.Recover()                 // every goroutine you start
	work()
}()
```

Feature flags (see [flags]({{ENDPOINT}}/docs/flags.md)):

```go
flags, err := vaka.EvaluateFlags(ctx, vaka.FlagRequest{DistinctID: userID})
ctx = vaka.WithFlags(ctx, flags)         // errors and events now carry the flag values
if flags.Enabled("new_checkout") { ... }
```

## 5. Verify

```sh
VAKA_URL={{ENDPOINT}} VAKA_KEY={{KEY}} vaka send-test-event
```

Then call `vaka.CaptureMessage(ctx, "vaka install test", vaka.LevelInfo)` from the app and check
`vaka issues -p {{PROJECT}}` (or the MCP tool `list_issues`). Events appear within a few seconds;
spans show up under `list_slow_traces`. Set `Debug: true` in `Config` to log what the SDK sends.
