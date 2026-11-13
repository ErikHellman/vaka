"""Flask integration.

    import vaka
    from vaka.integrations.flask import init_app

    vaka.init(...)
    app = Flask(__name__)
    init_app(app)

Each request runs in its own scope (breadcrumbs, tags and user don't leak between requests),
unhandled exceptions are reported with the route (``GET /carts/<int:id>``) as the transaction,
and errors join the caller's trace when a ``traceparent`` header is present.
"""

from __future__ import annotations

from typing import Any

from .._client import current_scope
from ._request import capture_request_error, set_route
from .wsgi import VakaWsgiMiddleware


def init_app(app: Any) -> Any:
    from flask import got_request_exception, request

    if getattr(app, "_vaka_installed", False):
        return app
    app.wsgi_app = VakaWsgiMiddleware(app.wsgi_app, mechanism="flask")

    @app.before_request
    def _vaka_route() -> None:
        rule = request.url_rule
        set_route(current_scope(), request.method, rule.rule if rule is not None else None)

    def _on_exception(sender: Any, exception: BaseException, **extra: Any) -> None:
        capture_request_error(exception, current_scope(), "flask")

    got_request_exception.connect(_on_exception, app, weak=False)
    app._vaka_installed = True
    return app
