# Install Vaka in a Python application

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.

## 1. Add the dependency

Add `vaka-sdk` to the project's dependencies (`pyproject.toml`, `requirements.txt` or the
lock-file tool the project uses):

```sh
pip install vaka-sdk
```

For a backend service that should also send traces and logs, use the `otel` extra:
`vaka-sdk[otel]`.

## 2. Initialise once, as early as possible

Put this where the process starts: the top of `main`, Django's `settings.py`, or the module that
creates the Flask or FastAPI app.

```python
import vaka

vaka.init(
    key="{{KEY}}",
    endpoint="{{ENDPOINT}}",
    release="2.4.0",              # the app version; read it from your package metadata or CI
    environment="production",
    service="my-service",         # backend services: the service name
)
```

`key`, `endpoint`, `release` and `environment` default to `VAKA_KEY`, `VAKA_ENDPOINT`,
`VAKA_RELEASE` and `VAKA_ENVIRONMENT`, so they can come from the deployment environment instead
of source code. Without a key the SDK is a no-op.

For the release, prefer the installed version:
`importlib.metadata.version("your-package")`, or the git SHA / build number from CI.

## 3. Add the framework integration

Pick the one that matches the app:

- **Django**: add `"vaka.integrations.django.VakaMiddleware"` as the first entry of `MIDDLEWARE`.
- **Flask**: `from vaka.integrations.flask import init_app` and call `init_app(app)` after creating the app.
- **FastAPI / Starlette**: `from vaka.integrations.asgi import VakaAsgiMiddleware` and `app.add_middleware(VakaAsgiMiddleware)`.
- **Other WSGI / ASGI apps**: wrap the app with `VakaWsgiMiddleware(app)` or `VakaAsgiMiddleware(app)`.
- **Workers, scripts, serverless handlers**: decorate the entry function with `@vaka.monitor`.

## 4. What you get automatically

- Uncaught exceptions in the main thread and in `threading` threads, with source context, sent
  before the process exits.
- `logging` errors (`logger.error`, `logger.exception`) as Vaka errors, and INFO+ records as
  breadcrumbs. Records only arrive if the logger's level lets them through.
- One session per process, so crash-free rates per release work.
- In web apps: unhandled request exceptions with the route, URL and the caller's trace id.

## 5. Report errors and events where it matters

```python
try:
    charge(order)
except PaymentError as e:
    vaka.capture_exception(e)         # handled errors you still want to see

vaka.add_breadcrumb("payment started", category="payment", data={"order": order.id})
vaka.set_tag("tenant", tenant.slug)
vaka.set_user(user.id)                # opt-in only: if the privacy policy allows it
vaka.track("order_placed", {"plan": "pro"})
```

## 6. Optional: traces and logs

With `vaka-sdk[otel]`, right after `vaka.init(...)`:

```python
import vaka.otel
vaka.otel.setup()
```

Then add the OpenTelemetry instrumentation packages for the frameworks and clients the app uses,
for example `opentelemetry-instrumentation-django`, `-flask`, `-fastapi`, `-requests`, `-httpx`,
`-psycopg` or `-sqlalchemy`, and call their `instrument()` functions. Spans appear in Vaka's
performance views (`slow_endpoints`, `get_trace`) and logs in `search_logs`.

## 7. Verify

```python
import vaka
vaka.init(key="{{KEY}}", endpoint="{{ENDPOINT}}", debug=True)
vaka.capture_message("vaka install test", level="info")
vaka.flush()
```

`debug=True` prints a `[vaka] sent … items (202)` line when it works. Then check
`vaka issues -p {{PROJECT}}` (or the MCP tool `list_issues`): the event appears within a few seconds.
