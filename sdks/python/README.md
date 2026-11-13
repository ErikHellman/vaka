# vaka-sdk (Python)

Python SDK for [Vaka](https://github.com/ErikHellman/vaka), the agent-first crash reporting and
analytics service: unhandled exceptions, `logging` errors, breadcrumbs, release health, custom
events and feature flags, plus one-call OpenTelemetry setup for traces and logs.

```sh
pip install vaka-sdk            # errors, sessions, events, flags (no dependencies)
pip install "vaka-sdk[otel]"    # + traces and logs through OpenTelemetry
```

```python
import vaka

vaka.init(key="vk_...", endpoint="https://vaka.example.com", release="2.4.0", service="checkout")

try:
    charge(order)
except PaymentError as e:
    vaka.capture_exception(e)

vaka.track("order_placed", {"plan": "pro"})
```

Framework hooks: `vaka.integrations.django.VakaMiddleware`, `vaka.integrations.flask.init_app(app)`,
`vaka.integrations.asgi.VakaAsgiMiddleware` (FastAPI, Starlette) and
`vaka.integrations.wsgi.VakaWsgiMiddleware`. Traces and logs: `vaka.otel.setup()`.

Full documentation: [docs/sdk-python.md](https://github.com/ErikHellman/vaka/blob/main/docs/sdk-python.md).
Agent-followable install guide: `/docs/install/python` on any Vaka server.

Python 3.9+. Licensed MIT OR Apache-2.0.
