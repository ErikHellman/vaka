"""Django integration.

Add the middleware first in ``settings.MIDDLEWARE`` and call ``vaka.init()`` in ``settings.py``
(or ``wsgi.py``/``asgi.py``):

    MIDDLEWARE = ["vaka.integrations.django.VakaMiddleware", ...]

Each request runs in its own scope, unhandled view exceptions are reported with the URL
pattern (``GET carts/<int:pk>/``) as the transaction, and errors join the caller's trace when a
``traceparent`` header is present. ``django.request`` error logs are not reported a second time.
"""

from __future__ import annotations

from typing import Any, Callable

from .._client import _current_scope, current_scope
from ._request import apply_request, capture_request_error, set_route


class VakaMiddleware:
    sync_capable = True
    async_capable = False

    def __init__(self, get_response: Callable[[Any], Any]):
        self.get_response = get_response

    def __call__(self, request: Any) -> Any:
        scope = current_scope().copy()
        token = _current_scope.set(scope)
        try:
            apply_request(scope, request.method, request.build_absolute_uri(), request.META.get("HTTP_TRACEPARENT"))
            return self.get_response(request)
        finally:
            _current_scope.reset(token)

    def process_view(self, request: Any, view_func: Any, view_args: Any, view_kwargs: Any) -> None:
        match = getattr(request, "resolver_match", None)
        route = getattr(match, "route", None) if match is not None else None
        if route:
            set_route(current_scope(), request.method, "/" + route.lstrip("^").rstrip("$").lstrip("/"))
        return None

    def process_exception(self, request: Any, exception: BaseException) -> None:
        capture_request_error(exception, current_scope(), "django")
        return None
