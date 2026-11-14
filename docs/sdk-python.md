# Python SDK

Package `vaka-sdk` on PyPI, imported as `vaka` (source: `sdks/python`). Python 3.9+. The core
has no dependencies: errors, sessions, breadcrumbs, events and flags go to the
[ingest API](ingest.md) from a background thread. Traces and logs go through the standard
OpenTelemetry SDK to Vaka's [OTLP endpoint](opentelemetry.md), set up with one call.

Agent-followable install guide: [`/docs/install/python`]({{ENDPOINT}}/docs/install/python).

```sh
pip install vaka-sdk            # errors, sessions, events, flags
pip install "vaka-sdk[otel]"    # + OpenTelemetry traces and logs
```

## Init

```python
import vaka

vaka.init(
    key="vk_...",                       # default $VAKA_KEY
    endpoint="https://vaka.example.com",  # default $VAKA_ENDPOINT
    release="2.4.0",                    # default $VAKA_RELEASE
    environment="production",           # default $VAKA_ENVIRONMENT or "production"
    service="checkout",                 # default $VAKA_SERVICE or $OTEL_SERVICE_NAME
)
```

Without a key and endpoint the SDK stays disabled and every call is a no-op.

| Option | Default | Meaning |
| --- | --- | --- |
| `capture_unhandled` | `True` | Report uncaught exceptions (`sys.excepthook`, `threading.excepthook`) |
| `sessions` | `True` | One session per process for crash-free rates per release |
| `logging` | `True` | `logging` records become breadcrumbs (INFO+) and errors (ERROR+) |
| `breadcrumb_level` / `event_level` | `logging.INFO` / `logging.ERROR` | Thresholds for the above; `event_level=None` turns off error capture from logs |
| `in_app_include` / `in_app_exclude` | empty | Module prefixes that are (not) your code. Default: everything outside the standard library and site-packages is in-app |
| `before_send` | `None` | `fn(item) -> item or None` to edit or drop anything before it is sent |
| `sample_rate` | `1.0` | Share of errors to send |
| `flags` | `False` | Load feature flags in the background at init |
| `flag_properties` | `{}` | Properties sent with every flag evaluation |
| `distinct_id` | random per process | Id used for sessions, events and flag bucketing |
| `flush_interval` | `5.0` | Seconds between batches (errors are sent immediately) |
| `debug` | `False` (or `$VAKA_DEBUG=1`) | Log SDK activity to stderr |

## API

```python
vaka.capture_exception(e)                 # or no argument inside `except`; returns the event id
vaka.capture_exception(e, level="warning", tags={"job": "import"}, fingerprint=["import-timeout"])
vaka.capture_message("cache rebuilt", level="info")
vaka.add_breadcrumb("cart loaded", category="db", data={"rows": 3})
vaka.set_tag("region", "eu-north-1")
vaka.set_context("job", {"id": 42, "queue": "imports"})
vaka.set_user("user-123")                 # opt-in; vaka.set_user(None) clears
vaka.track("order_placed", {"plan": "pro", "items": 3})
vaka.flush(2.0)
vaka.shutdown()                           # also runs at interpreter exit
```

Exception chains (`raise ... from ...` and implicit context) are sent outermost first, each with
frames crash site first, source context lines and `in_app` marks. Uncaught exceptions are sent
synchronously before the interpreter exits and end the session as `crashed`. The same exception
object is only reported once, so a framework that both raises and logs an error doesn't create two
events.

### Scopes

Tags, breadcrumbs, user, contexts and flag exposures live on a scope. The framework integrations
give every request its own copy; for jobs and tasks use:

```python
with vaka.isolation_scope():
    vaka.set_tag("job", "nightly-import")
    run_import()

@vaka.monitor            # isolation scope + report and re-raise + flush
def handle(event, context): ...
```

Scopes follow `contextvars`, so each asyncio task keeps its own. A new thread starts on the
global scope; use `contextvars.copy_context().run` to carry a request scope into one.

## Cron jobs (heartbeat monitors)

Report each run of a job to a [heartbeat monitor](monitoring.md). Vaka alerts when a run is
missed, fails or runs too long.

```python
@vaka.monitor("nightly-backup", schedule="0 3 * * *", timezone="Europe/Stockholm")
def backup(): ...

with vaka.monitor("hourly-sync", every="1h"):   # also `async with`
    sync()

vaka.check_in("hourly-sync")                     # a single ok
vaka.check_in("hourly-sync", "error", message="upstream down")
```

A monitored run reports `in_progress`, then `ok` with the duration, or `error` when an exception
escapes (it is captured and re-raised). With a schedule, the monitor is created on first use.
Errors captured during the run carry the tag `monitor.check_in_id`. `check_in` returns `False`
instead of raising when the check-in fails. Plain `@vaka.monitor` without a slug still only
reports exceptions.

## User feedback

Forward what users wrote in your own forms (support, contact, in-app chat). It is sent right away
(a blocking call) with the scope's user, tags and flags.

```python
r = vaka.capture_feedback(
    form["message"],
    email=form.get("email"),          # optional contact
    issue="API-12",                   # or event_id=vaka.capture_exception(e)
    ip=request.remote_addr,
    attachments=[("screenshot.png", data)],
)
print(r.short_id)                     # "API-F12"
```

Keyword arguments: `email`, `name`, `event_id`, `issue`, `user_id`, `session_id`, `url`, `ip`,
`tags`, `attachments` (up to 3 `(filename, bytes[, content_type])` PNG, JPEG or WebP images of
5 MB), `timeout`. It raises `vaka.FeedbackError` with `.status` (429: rate limited). In async code
call it with `await asyncio.to_thread(vaka.capture_feedback, ...)`.

Link it to a crash with the event id from a capture call, or to an issue by its short id (`API-12`). Pass the end user's IP address: feedback is rate limited per address (10 an hour by default), so without it all feedback from your server shares one limit. Vaka believes the address only from backends on a private network or listed in `VAKA_TRUSTED_PROXIES`. Rejected feedback raises an error with the HTTP status; 429 means a limit was hit. See [user feedback](feedback.md).

## Frameworks

| Framework | Setup |
| --- | --- |
| Django | `MIDDLEWARE = ["vaka.integrations.django.VakaMiddleware", ...]` (first), `vaka.init()` in `settings.py` |
| Flask | `from vaka.integrations.flask import init_app; init_app(app)` |
| FastAPI, Starlette | `app.add_middleware(VakaAsgiMiddleware)` from `vaka.integrations.asgi` |
| Other ASGI apps | `app = VakaAsgiMiddleware(app)` |
| Other WSGI apps | `app = VakaWsgiMiddleware(app)` from `vaka.integrations.wsgi` |

Each request gets its own scope with the method and URL (no headers, cookies or bodies), the
route template as the `transaction` tag (`GET /carts/{cart_id}`), and the trace id from an
incoming `traceparent` header, so a backend error links to the browser trace that caused it.
Unhandled exceptions are reported with `handled: false`. Exceptions the framework turns into
responses (`HTTPException`, `Http404`) are not reported.

## Traces and logs (OpenTelemetry)

```python
import vaka, vaka.otel

vaka.init(key="vk_...", endpoint="https://vaka.example.com", release="2.4.0", service="checkout")
vaka.otel.setup()      # traces and logs; setup(logs=False) for traces only
```

`setup()` configures a `TracerProvider` and `LoggerProvider` with OTLP/HTTP exporters pointed at
`{{ENDPOINT}}/ingest/otlp` and resource attributes from the Vaka client (`service.name`,
`service.version` = release, `deployment.environment.name`), so spans, logs and errors line up per
release. Then use any OpenTelemetry instrumentation:

```sh
pip install opentelemetry-instrumentation-fastapi opentelemetry-instrumentation-requests
```

```python
from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor
FastAPIInstrumentor.instrument_app(app)
```

- Spans get `session.id` and `enduser.id` from the Vaka scope.
- Errors captured while a span is active carry its `trace_id`, and the span is marked
  `vaka.error_captured`, so Vaka doesn't create a second error from the span's exception event.
  Log records the SDK already reported as errors are marked the same way.
- Log records at `log_level` (INFO) and above are exported. Adding the OpenTelemetry handler to
  the root logger means Python's last-resort stderr output stops if the app had no handlers of its
  own; call `logging.basicConfig()` to keep console output.
- `setup(set_global=False)` returns `(tracer_provider, logger_provider)` without installing them,
  for apps that configure OpenTelemetry themselves.

Already using OpenTelemetry with environment variables or the Collector? Keep that setup (see
[opentelemetry.md](opentelemetry.md)) and use the SDK for errors only.

## Feature flags

```python
vaka.load_flags({"plan": "pro"})             # evaluate once for this process
if vaka.is_enabled("new_checkout"): ...
variant = vaka.get_flag("checkout_layout", "control")
```

On a server, evaluate per person inside the request:

```python
flags = vaka.evaluate_flags(distinct_id=user.id, user_id=user.id, properties={"plan": user.plan})
if flags.get("new_checkout") == "one_page": ...
```

Every value read with `get_flag`, `is_enabled` or `Flags.get` is recorded on the current scope
and sent as `flags` on errors and events (and on the process session), so `flag_health` and
experiment results can split crash rates and conversions by variant. `Flags.peek(key)` reads
without recording. See [flags.md](flags.md).

## Releasing

Maintainers: see [releasing/python.md](https://github.com/ErikHellman/vaka/blob/main/docs/releasing/python.md)
for publishing to PyPI.
