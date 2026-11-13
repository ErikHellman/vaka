"""WSGI middleware: one isolation scope per request, unhandled exceptions reported.

    from vaka.integrations.wsgi import VakaWsgiMiddleware
    application = VakaWsgiMiddleware(application)
"""

from __future__ import annotations

from typing import Any, Callable, Iterable, Iterator, Optional

from .._client import Scope, _current_scope, isolation_scope
from ._request import apply_request, capture_request_error


def request_url(environ: Any) -> str:
    scheme = environ.get("wsgi.url_scheme", "http")
    host = environ.get("HTTP_HOST") or f"{environ.get('SERVER_NAME', '')}:{environ.get('SERVER_PORT', '')}"
    path = (environ.get("SCRIPT_NAME", "") or "") + (environ.get("PATH_INFO", "") or "")
    qs = environ.get("QUERY_STRING")
    return f"{scheme}://{host}{path}" + (f"?{qs}" if qs else "")


class VakaWsgiMiddleware:
    def __init__(self, app: Callable[..., Iterable[bytes]], mechanism: str = "wsgi"):
        self.app = app
        self.mechanism = mechanism

    def __call__(self, environ: Any, start_response: Callable[..., Any]) -> Iterable[bytes]:
        with isolation_scope() as scope:
            apply_request(scope, environ.get("REQUEST_METHOD", "GET"), request_url(environ), environ.get("HTTP_TRACEPARENT"))
            environ["vaka.scope"] = scope
            try:
                result = self.app(environ, start_response)
            except Exception as e:
                capture_request_error(e, scope, self.mechanism)
                raise
        return _ScopedIterable(result, scope, self.mechanism)


class _ScopedIterable:
    """Keeps the request scope active while a streaming response is iterated."""

    def __init__(self, inner: Iterable[bytes], scope: Scope, mechanism: str):
        self.inner = inner
        self.scope = scope
        self.mechanism = mechanism

    def __iter__(self) -> Iterator[bytes]:
        token = _current_scope.set(self.scope)
        try:
            it = iter(self.inner)
            while True:
                try:
                    chunk = next(it)
                except StopIteration:
                    return
                except Exception as e:
                    capture_request_error(e, self.scope, self.mechanism)
                    raise
                yield chunk
        finally:
            _current_scope.reset(token)

    def close(self) -> None:
        close: Optional[Callable[[], None]] = getattr(self.inner, "close", None)
        if close is not None:
            close()
