# Go SDK

Module `github.com/ErikHellman/vaka/sdks/go` (source: `sdks/go`, package `vaka`). A thin layer
on the standard OpenTelemetry Go SDK: Init installs OpenTelemetry tracer and logger providers that
export OTLP/HTTP to Vaka ([opentelemetry.md](opentelemetry.md)), and adds what OpenTelemetry does
not do: panic and error capture with stack traces, breadcrumbs, release health sessions, user
context, analytics events and feature flags, sent to the [ingest API](ingest.md). Go 1.24+.

Agent-followable install guide: [`/docs/install/go`]({{ENDPOINT}}/docs/install/go).

```sh
go get github.com/ErikHellman/vaka/sdks/go@latest
```

## Config

| Field | Default | Meaning |
| --- | --- | --- |
| `Key` | `$VAKA_KEY` | Ingest key; empty disables the SDK |
| `Endpoint` | `$VAKA_ENDPOINT` | Server URL |
| `Release` | `$VAKA_RELEASE`, then the module version or VCS revision from the build info | App version (`service.version`) |
| `Environment` | `$VAKA_ENVIRONMENT` or `production` | `deployment.environment.name` |
| `ServiceName` | `$OTEL_SERVICE_NAME`, then the executable name | `service.name` |
| `DisableTracing` / `DisableLogs` | `false` | Don't install the global OpenTelemetry TracerProvider / LoggerProvider (use your own) |
| `TracesSampler` | parent-based, always on | OpenTelemetry sampler |
| `DisableSessions` | `false` | One session per process for crash-free rates |
| `InAppInclude` | main module path | Package path prefixes that count as your code |
| `FlushInterval` | 5 s | Batch interval |
| `BeforeSend` | nil | Change or drop (return nil) any item |
| `Debug` | `false` | Log SDK activity to stderr |

## API

```go
client, err := vaka.Init(cfg)                  // defer client.Shutdown(ctx): ends the session, flushes
defer vaka.Recover()                           // report a panic, mark the session crashed, re-panic

vaka.CaptureError(ctx, err)                    // Unwrap chain, stack, breadcrumbs, trace id from ctx
vaka.CaptureMessage(ctx, "text", vaka.LevelWarning)
vaka.CaptureError(ctx, err, vaka.WithTags(map[string]string{"job": "import"}), vaka.WithFingerprint("import"))

ctx = vaka.WithScope(ctx)                      // per-request scope: user, tags, flags, breadcrumbs
vaka.AddBreadcrumb(ctx, "db", "connected", vaka.LevelInfo)
vaka.SetTag(ctx, "region", "eu-north-1")
vaka.SetUser(ctx, "user-123")                  // opt-in
vaka.Track(ctx, "job_finished", map[string]any{"kind": "import"})

flags, err := vaka.EvaluateFlags(ctx, vaka.FlagRequest{DistinctID: userID, Properties: map[string]any{"plan": "pro"}})
ctx = vaka.WithFlags(ctx, flags)               // exposure: flag values go on errors and events
if flags.Variant("new_checkout", "control") == "one_page" { ... }

vaka.Go(ctx, func(ctx context.Context) { ... }) // goroutine that reports its panic
```

Without `WithScope`, these use the client's global scope. Calls on a disabled (no key) or nil
client are no-ops.

## HTTP, logs and traces

- `vaka.Middleware(handler)`: per-request scope, a SERVER span that continues an incoming
  `traceparent` and is named after the `http.ServeMux` pattern (`GET /carts/{id}`), and panic
  reporting (answers 500; `MiddlewareWithOptions{Repanic: true}` re-panics). Set `DisableSpans`
  when `otelhttp` or a framework already makes the server span.
- `vaka.Transport(nil)`: `http.RoundTripper` with a CLIENT span, `traceparent` injection and an
  `http` breadcrumb.
- `vaka.SlogHandler(next)`: a `log/slog` handler. Records become breadcrumbs, ERROR records become
  Vaka errors (an `error`-valued attribute is the exception), and every record is exported to Vaka's
  log search through OpenTelemetry, linked to the trace. `next` (for example a `slog.TextHandler`)
  still gets every record.
- `vaka.StartSpan(ctx, name)` / `vaka.Tracer()`: spans with the global TracerProvider. Any other
  OpenTelemetry instrumentation (`otelhttp`, `otelgrpc`, `otelsql`) exports to Vaka too.

Errors the SDK sends itself mark the active span and log record with `vaka.error_captured: true`,
so the server does not create a second error from the OTLP data.

## Panics

Go cannot catch panics globally, so add `defer vaka.Recover()` at the top of `main` and of each
goroutine you start (or start them with `vaka.Go`). `Recover` sends synchronously (up to 2 s) and
re-panics, so the program still exits the same way. Frames are resolved in-process, crash site
first; paths below your module root are sent as repository paths (`cart/checkout.go`) so source
links and suspect commits work. No debug file upload is needed.

See `sdks/go/examples/demo` for a runnable example.
