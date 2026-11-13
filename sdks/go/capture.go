package vaka

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"reflect"
	"runtime"
	"strings"
	"sync"
	"time"

	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/trace"
)

func newID() string {
	var b [16]byte
	_, _ = rand.Read(b[:])
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	h := hex.EncodeToString(b[:])
	return h[:8] + "-" + h[8:12] + "-" + h[12:16] + "-" + h[16:20] + "-" + h[20:]
}

func nowMs() int64 { return time.Now().UnixMilli() }

// EventOption changes a captured error or message before it is sent.
type EventOption func(item map[string]any)

// WithTags adds tags to one event.
func WithTags(tags map[string]string) EventOption {
	return func(item map[string]any) {
		t, _ := item["tags"].(map[string]string)
		if t == nil {
			t = map[string]string{}
		}
		for k, v := range tags {
			t[k] = v
		}
		item["tags"] = t
	}
}

// WithFingerprint sets a custom grouping key for one event.
func WithFingerprint(parts ...string) EventOption {
	return func(item map[string]any) { item["fingerprint"] = parts }
}

// WithLevel sets the level of one event.
func WithLevel(l Level) EventOption {
	return func(item map[string]any) {
		item["level"] = string(l)
		item["fatal"] = l == LevelFatal
	}
}

// stackTracer is implemented by errors that carry the program counters of where they were
// created (for example github.com/go-errors/errors and cockroachdb/errors).
type stackTracer interface{ Callers() []uintptr }

var wrapperTypes = map[string]bool{
	"*fmt.wrapError":    true,
	"*fmt.wrapErrors":   true,
	"*errors.joinError": true,
}

func typeName(err error) string {
	t := reflect.TypeOf(err).String()
	if t == "*errors.errorString" {
		return "error"
	}
	return strings.TrimPrefix(t, "*")
}

// exceptions turns an error and its Unwrap chain into Vaka's exception list, outermost
// first. fmt.Errorf wrappers take the type of the error they wrap, so grouping uses the
// meaningful type (*fs.PathError) rather than *fmt.wrapError.
func (c *Client) exceptions(err error, stack []Frame) []map[string]any {
	var chain []error
	for e := err; e != nil && len(chain) < 10; {
		chain = append(chain, e)
		switch u := e.(type) {
		case interface{ Unwrap() error }:
			e = u.Unwrap()
		case interface{ Unwrap() []error }:
			errs := u.Unwrap()
			if len(errs) == 0 {
				e = nil
			} else {
				e = errs[0]
			}
		default:
			e = nil
		}
	}
	out := make([]map[string]any, 0, len(chain))
	for i, e := range chain {
		typ := typeName(e)
		if wrapperTypes[reflect.TypeOf(e).String()] {
			typ = "error"
			for _, inner := range chain[i+1:] {
				if !wrapperTypes[reflect.TypeOf(inner).String()] {
					typ = typeName(inner)
					break
				}
			}
		}
		ex := map[string]any{"type": typ, "value": e.Error()}
		if pkg := reflect.TypeOf(e); pkg != nil {
			t := pkg
			for t.Kind() == reflect.Pointer {
				t = t.Elem()
			}
			if t.PkgPath() != "" {
				ex["module"] = t.PkgPath()
			}
		}
		if st, ok := e.(stackTracer); ok && i > 0 {
			ex["stacktrace"] = trimLeading(c.framesFromPCs(st.Callers()))
		}
		out = append(out, ex)
	}
	// The outermost exception gets the most specific stack we have: the innermost error's
	// own stack if it recorded one, otherwise where it was captured.
	if len(out) > 0 {
		var own []Frame
		for i := len(chain) - 1; i >= 0; i-- {
			if st, ok := chain[i].(stackTracer); ok {
				own = trimLeading(c.framesFromPCs(st.Callers()))
				break
			}
		}
		if own != nil {
			out[0]["stacktrace"] = own
		} else {
			out[0]["stacktrace"] = stack
		}
	}
	return out
}

func (c *Client) errorItem(ctx context.Context, level Level, handled bool, message string, exceptions []map[string]any) map[string]any {
	if ctx == nil {
		ctx = context.Background()
	}
	item := map[string]any{
		"type":        "error",
		"event_id":    newID(),
		"timestamp":   nowMs(),
		"platform":    "go",
		"level":       string(level),
		"handled":     handled,
		"fatal":       level == LevelFatal,
		"release":     c.cfg.Release,
		"environment": c.cfg.Environment,
		"contexts": map[string]any{
			"runtime": map[string]any{"name": "go", "version": runtime.Version()},
			"os":      map[string]any{"name": runtime.GOOS, "arch": runtime.GOARCH},
		},
		"tags": map[string]string{"service": c.cfg.ServiceName},
	}
	if message != "" {
		item["message"] = message
	}
	if len(exceptions) > 0 {
		item["exceptions"] = exceptions
	}
	if sc := trace.SpanContextFromContext(ctx); sc.HasTraceID() {
		item["trace_id"] = sc.TraceID().String()
		item["span_id"] = sc.SpanID().String()
		// The error is sent here with its stack; tell the server not to derive another one
		// from this span's OTLP exception events.
		trace.SpanFromContext(ctx).SetAttributes(attribute.Bool(CapturedAttr, true))
	}
	c.addSession(item, true)
	ScopeFromContext(ctx).apply(item, true)
	if c.scope != ScopeFromContext(ctx) {
		c.scope.apply(item, false)
	}
	return item
}

// CaptureError reports an error with the current stack and returns the event id ("" when
// the SDK is disabled or err is nil). ctx supplies the scope (see WithScope) and the
// OpenTelemetry span, whose trace id links the error to its trace.
func (c *Client) CaptureError(ctx context.Context, err error, opts ...EventOption) string {
	if !c.enabled() || err == nil {
		return ""
	}
	stack := trimLeading(c.Stacktrace(1))
	item := c.errorItem(ctx, LevelError, true, "", c.exceptions(err, stack))
	return c.finish(item, opts)
}

// CaptureMessage reports a message without an error value.
func (c *Client) CaptureMessage(ctx context.Context, message string, level Level, opts ...EventOption) string {
	if !c.enabled() {
		return ""
	}
	item := c.errorItem(ctx, level, true, message, nil)
	return c.finish(item, opts)
}

func (c *Client) finish(item map[string]any, opts []EventOption) string {
	for _, o := range opts {
		o(item)
	}
	c.enqueue(item)
	id, _ := item["event_id"].(string)
	return id
}

// capturePanic reports a recovered panic value as an unhandled error.
func (c *Client) capturePanic(ctx context.Context, v any, level Level) string {
	if !c.enabled() {
		return ""
	}
	stack := trimLeading(c.Stacktrace(2))
	var exs []map[string]any
	if err, ok := v.(error); ok {
		exs = c.exceptions(err, stack)
		var re runtime.Error
		if errors.As(err, &re) {
			exs[0]["type"] = "panic: " + typeName(re)
		}
	} else {
		exs = []map[string]any{{"type": "panic", "value": fmt.Sprint(v), "stacktrace": stack}}
	}
	item := c.errorItem(ctx, level, false, "", exs)
	item["tags"].(map[string]string)["mechanism"] = "panic"
	return c.finish(item, nil)
}

// Recover reports a panic, marks the session crashed, flushes (up to 2 s) and re-panics,
// so the program still crashes as it would without Vaka. Use it as `defer vaka.Recover()`
// at the top of main and of every goroutine you start: Go cannot catch panics globally.
func (c *Client) Recover() {
	if v := recover(); v != nil {
		c.crash(context.Background(), v)
		panic(v)
	}
}

// RecoverContext is Recover with a context for the scope and trace.
func (c *Client) RecoverContext(ctx context.Context) {
	if v := recover(); v != nil {
		c.crash(ctx, v)
		panic(v)
	}
}

func (c *Client) crash(ctx context.Context, v any) {
	if !c.enabled() {
		return
	}
	c.capturePanic(ctx, v, LevelFatal)
	c.endSession("crashed")
	fctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_ = c.Flush(fctx)
}

// Track sends a custom analytics event with flat string, number or bool props.
func (c *Client) Track(ctx context.Context, name string, props map[string]any) {
	if !c.enabled() {
		return
	}
	if ctx == nil {
		ctx = context.Background()
	}
	item := map[string]any{
		"type":        "event",
		"name":        name,
		"timestamp":   nowMs(),
		"platform":    "go",
		"release":     c.cfg.Release,
		"environment": c.cfg.Environment,
	}
	if len(props) > 0 {
		item["props"] = props
	}
	c.addSession(item, false)
	ScopeFromContext(ctx).apply(item, false)
	if c.scope != ScopeFromContext(ctx) {
		c.scope.apply(item, false)
	}
	c.enqueue(item)
}

// --- sessions -------------------------------------------------------------

type session struct {
	mu      sync.Mutex
	id      string
	anonID  string
	started time.Time
	errors  int
	ended   bool
}

func (c *Client) startSession() {
	c.session = &session{id: newID(), anonID: newID(), started: time.Now()}
	c.enqueue(c.sessionItem("ok"))
}

func (c *Client) addSession(item map[string]any, isError bool) {
	s := c.session
	if s == nil {
		return
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	item["session_id"] = s.id
	item["anon_id"] = s.anonID
	if isError && !s.ended {
		s.errors++
	}
}

func (c *Client) sessionItem(status string) map[string]any {
	s := c.session
	item := map[string]any{
		"type":        "session",
		"session_id":  s.id,
		"status":      status,
		"started":     s.started.UnixMilli(),
		"timestamp":   nowMs(),
		"duration_ms": time.Since(s.started).Milliseconds(),
		"errors":      s.errors,
		"release":     c.cfg.Release,
		"environment": c.cfg.Environment,
		"platform":    "go",
		"anon_id":     s.anonID,
	}
	c.scope.apply(item, false)
	return item
}

func (c *Client) endSession(status string) {
	s := c.session
	if s == nil || !c.enabled() {
		return
	}
	s.mu.Lock()
	if s.ended {
		s.mu.Unlock()
		return
	}
	s.ended = true
	if status == "exited" && s.errors > 0 {
		status = "errored"
	}
	s.mu.Unlock()
	c.enqueue(c.sessionItem(status))
}

// --- package-level shortcuts for the current client -------------------------

// CaptureError reports err with the current client. See Client.CaptureError.
func CaptureError(ctx context.Context, err error, opts ...EventOption) string {
	return CurrentClient().CaptureError(ctx, err, opts...)
}

// CaptureMessage reports a message with the current client.
func CaptureMessage(ctx context.Context, message string, level Level, opts ...EventOption) string {
	return CurrentClient().CaptureMessage(ctx, message, level, opts...)
}

// Track sends an analytics event with the current client.
func Track(ctx context.Context, name string, props map[string]any) {
	CurrentClient().Track(ctx, name, props)
}

// Recover reports a panic with the current client and re-panics. Use `defer vaka.Recover()`.
func Recover() {
	if v := recover(); v != nil {
		CurrentClient().crash(context.Background(), v)
		panic(v)
	}
}

// RecoverContext is Recover with a context for the scope and trace.
func RecoverContext(ctx context.Context) {
	if v := recover(); v != nil {
		CurrentClient().crash(ctx, v)
		panic(v)
	}
}

// Go runs fn in a new goroutine that reports a panic before crashing the program.
func Go(ctx context.Context, fn func(ctx context.Context)) {
	go func() {
		defer RecoverContext(ctx)
		fn(ctx)
	}()
}
