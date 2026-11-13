"""ASGI middleware for FastAPI, Starlette, Quart, Django ASGI and others.

    from vaka.integrations.asgi import VakaAsgiMiddleware
    app.add_middleware(VakaAsgiMiddleware)      # FastAPI / Starlette
    # or: application = VakaAsgiMiddleware(application)

Each request runs in its own scope, unhandled exceptions are reported (with the route template
such as ``GET /carts/{cart_id}`` when the framework exposes it), and errors join the caller's
trace when a ``traceparent`` header is present.
"""

from __future__ import annotations

from typing import Any, Awaitable, Callable, Dict, MutableMapping, Optional

from .._client import isolation_scope
from ._request import apply_request, capture_request_error, set_route

Receive = Callable[[], Awaitable[MutableMapping[str, Any]]]
Send = Callable[[MutableMapping[str, Any]], Awaitable[None]]


def _headers(scope: MutableMapping[str, Any]) -> Dict[str, str]:
    out: Dict[str, str] = {}
    for k, v in scope.get("headers") or []:
        try:
            out[k.decode("latin-1").lower()] = v.decode("latin-1")
        except Exception:  # pragma: no cover
            pass
    return out


def request_url(scope: MutableMapping[str, Any], headers: Dict[str, str]) -> str:
    scheme = scope.get("scheme", "http")
    host = headers.get("host")
    if not host:
        server = scope.get("server") or ("", None)
        host = f"{server[0]}:{server[1]}" if server[1] else str(server[0])
    path = (scope.get("root_path", "") or "") + (scope.get("path", "") or "")
    qs = scope.get("query_string") or b""
    if isinstance(qs, bytes):
        qs = qs.decode("latin-1")
    return f"{scheme}://{host}{path}" + (f"?{qs}" if qs else "")


def _route(scope: MutableMapping[str, Any]) -> Optional[str]:
    route = scope.get("route")
    path = getattr(route, "path", None) or getattr(route, "path_format", None)
    return path if isinstance(path, str) else None


class VakaAsgiMiddleware:
    def __init__(self, app: Callable[..., Awaitable[None]], mechanism: str = "asgi"):
        self.app = app
        self.mechanism = mechanism

    async def __call__(self, scope: MutableMapping[str, Any], receive: Receive, send: Send) -> None:
        if scope.get("type") not in ("http", "websocket"):
            await self.app(scope, receive, send)
            return
        headers = _headers(scope)
        method = scope.get("method", "WS" if scope.get("type") == "websocket" else "GET")
        with isolation_scope() as vscope:
            apply_request(vscope, method, request_url(scope, headers), headers.get("traceparent"))
            try:
                await self.app(scope, receive, send)
            except Exception as e:
                set_route(vscope, method, _route(scope))
                capture_request_error(e, vscope, self.mechanism)
                raise
