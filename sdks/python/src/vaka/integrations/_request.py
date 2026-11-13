"""Shared helpers for the web framework integrations."""

from __future__ import annotations

import re
from typing import Any, Optional

from .._client import Scope

_TRACEPARENT = re.compile(r"^[0-9a-f]{2}-([0-9a-f]{32})-([0-9a-f]{16})-[0-9a-f]{2}$")


def trace_id_from_traceparent(value: Optional[str]) -> Optional[str]:
    if not value:
        return None
    m = _TRACEPARENT.match(value.strip().lower())
    if not m or m.group(1) == "0" * 32:
        return None
    return m.group(1)


def apply_request(scope: Scope, method: str, url: str, traceparent: Optional[str] = None) -> None:
    """Fill a fresh request scope. Only the method and URL are kept: no headers, cookies or bodies."""
    scope.breadcrumbs = []
    scope.url = url
    scope.tags["http.method"] = method
    scope.contexts["request"] = {"method": method, "url": url}
    tid = trace_id_from_traceparent(traceparent)
    if tid:
        scope.trace_id = tid


def set_route(scope: Scope, method: str, route: Optional[str]) -> None:
    if route:
        scope.transaction = f"{method} {route}"


def capture_request_error(exc: BaseException, scope: Scope, mechanism: str) -> None:
    from .. import _api

    c = _api.get_client()
    if c is not None:
        c.capture_exception(exc, handled=False, mechanism=mechanism, scope=scope)


def header(environ_or_headers: Any, name: str) -> Optional[str]:
    try:
        return environ_or_headers.get(name)
    except Exception:  # pragma: no cover
        return None
