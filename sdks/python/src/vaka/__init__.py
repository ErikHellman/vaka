"""Vaka SDK for Python: crash reporting, release health, analytics and feature flags.

    import vaka
    vaka.init(key="vk_...", endpoint="https://vaka.example.com", release="2.4.0")

Unhandled exceptions (main thread and ``threading`` threads), ``logging`` errors and one session
per process are reported automatically. Traces and logs go through OpenTelemetry: see
``vaka.otel.setup()``. Docs: https://vaka.hellsoft.se/docs/sdk-python
"""

from ._api import (
    add_breadcrumb,
    capture_exception,
    capture_feedback,
    capture_message,
    check_in,
    evaluate_flags,
    flush,
    get_client,
    get_flag,
    identify,
    init,
    is_enabled,
    is_initialized,
    load_flags,
    monitor,
    set_context,
    set_tag,
    set_tags,
    set_user,
    shutdown,
    track,
)
from ._client import Client, Options, Scope, current_scope, isolation_scope
from ._version import __version__
from .feedback import FeedbackError, FeedbackResult
from .flags import Flags

__all__ = [
    "__version__",
    "init",
    "get_client",
    "is_initialized",
    "capture_exception",
    "capture_message",
    "capture_feedback",
    "check_in",
    "FeedbackError",
    "FeedbackResult",
    "add_breadcrumb",
    "set_user",
    "identify",
    "set_tag",
    "set_tags",
    "set_context",
    "track",
    "flush",
    "shutdown",
    "monitor",
    "load_flags",
    "evaluate_flags",
    "get_flag",
    "is_enabled",
    "isolation_scope",
    "current_scope",
    "Client",
    "Options",
    "Scope",
    "Flags",
]
