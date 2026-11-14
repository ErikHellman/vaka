"""Module-level API (``vaka.init``, ``vaka.capture_exception`` …) around one global client."""

from __future__ import annotations

import sys
import threading
from typing import Any, Callable, Dict, Mapping, Optional, Sequence, TypeVar

from . import feedback as _feedback
from . import flags as _flags
from . import monitors as _monitors
from ._client import Client, Options, Scope, current_scope, isolation_scope, now_ms, MAX_BREADCRUMBS, _flag_str

_client: Optional[Client] = None
_lock = threading.Lock()

F = TypeVar("F", bound=Callable[..., Any])


def init(key: Optional[str] = None, endpoint: Optional[str] = None, **options: Any) -> Optional[Client]:
    """Start the SDK. Call once, as early as possible.

    ``key`` and ``endpoint`` default to ``$VAKA_KEY`` and ``$VAKA_ENDPOINT``; without both the SDK
    stays disabled and every call is a no-op. See ``vaka.Options`` for the other settings.
    Calling ``init`` again replaces the previous client (it is flushed and closed first).
    """
    global _client
    opts = Options(key=key, endpoint=endpoint, **options)
    if not opts.key or not opts.endpoint:
        if opts.debug or key or endpoint:
            print("[vaka] init: `key` and `endpoint` are required; SDK disabled", file=sys.stderr)
        return None
    with _lock:
        old, _client = _client, None
    if old is not None:
        old.close(timeout=2.0)
    client = Client(opts)
    with _lock:
        _client = client
    client.install_hooks()
    if opts.sessions:
        client.start_session()
    if opts.flags:
        threading.Thread(target=_load_flags_quietly, name="vaka-flags", daemon=True).start()
    client.log(f"initialized (release={opts.release}, environment={opts.environment})")
    return client


def get_client() -> Optional[Client]:
    return _client


def is_initialized() -> bool:
    return _client is not None


def capture_exception(exc: Optional[BaseException] = None, **kwargs: Any) -> Optional[str]:
    """Report an exception (default: the one being handled). Returns the event id.

    Keyword arguments: ``level`` ("error"), ``tags``, ``contexts``, ``fingerprint``, ``handled``.
    """
    c = _client
    return c.capture_exception(exc, **kwargs) if c else None


def capture_message(message: str, level: str = "info", **kwargs: Any) -> Optional[str]:
    """Report a message with the current stack. Returns the event id."""
    c = _client
    return c.capture_message(message, level=level, **kwargs) if c else None


def capture_feedback(
    message: str,
    *,
    email: Optional[str] = None,
    name: Optional[str] = None,
    event_id: Optional[str] = None,
    issue: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    url: Optional[str] = None,
    ip: Optional[str] = None,
    tags: Optional[Mapping[str, Any]] = None,
    attachments: Optional[Sequence[_feedback.Attachment]] = None,
    timeout: float = 15.0,
) -> _feedback.FeedbackResult:
    """Forward feedback a user wrote (a support form, in-app chat) to Vaka. Sent right away.

    ``event_id`` (from ``capture_exception``) or ``issue`` (id or short id like ``API-12``) link it
    to a crash. ``user_id``, ``url`` and tags default to the current scope's. ``ip`` is the end
    user's address: feedback is rate limited per address, so without it all feedback from this
    server shares one limit. ``attachments``: up to 3 ``(filename, bytes[, content_type])`` images.

    Returns ``FeedbackResult(id, short_id)``. Raises ``vaka.FeedbackError`` (with ``status``) when
    the SDK is not initialized or the server refuses the feedback, e.g. 429 for a rate limit.
    """
    c = _client
    if c is None:
        raise _feedback.FeedbackError("vaka is not initialized")
    scope = current_scope()
    common = {"platform": "python", "environment": c.options.environment, "timestamp": now_ms(), "anon_id": c.distinct_id}
    if c.options.release:
        common["release"] = c.options.release
    if scope.tags:
        common["tags"] = dict(scope.tags)
    if scope.flags:
        common["flags"] = {k: _flag_str(v) for k, v in list(scope.flags.items())[:32]}
    item = _feedback.build_item(
        message, common, email=email, name=name, event_id=event_id, issue=issue,
        user_id=user_id or scope.user_id, session_id=session_id, url=url or scope.url, tags=tags,
    )
    result = _feedback.send(c.options.endpoint, c.options.key, item, attachments, ip=ip, timeout=timeout)
    add_breadcrumb("user feedback sent", category="feedback")
    return result


def add_breadcrumb(
    message: str,
    category: str = "default",
    level: str = "info",
    data: Optional[Mapping[str, Any]] = None,
    type: Optional[str] = None,
) -> None:
    """Record a breadcrumb; the last 100 are sent with the next error.

    Categories with special meaning: ``navigation`` (``data: {from, to}``), ``http`` (``data:
    {method, url, status_code, duration_ms}``), ``console``, ``lifecycle``.
    """
    if _client is None:
        return
    crumb: Dict[str, Any] = {"timestamp": now_ms(), "category": category, "message": message, "level": level}
    if data:
        crumb["data"] = dict(data)
    if type:
        crumb["type"] = type
    crumbs = current_scope().breadcrumbs
    crumbs.append(crumb)
    if len(crumbs) > MAX_BREADCRUMBS:
        del crumbs[: len(crumbs) - MAX_BREADCRUMBS]


def set_user(user_id: Optional[str]) -> None:
    """Set (or clear with ``None``) the identified user. Opt-in: only if your privacy policy allows."""
    current_scope().user_id = str(user_id) if user_id is not None else None


identify = set_user


def set_tag(key: str, value: Any) -> None:
    """Set a tag sent with every error from the current scope."""
    current_scope().tags[key] = str(value)


def set_tags(tags: Mapping[str, Any]) -> None:
    for k, v in tags.items():
        set_tag(k, v)


def set_context(name: str, value: Mapping[str, Any]) -> None:
    """Attach structured context (shown on errors under ``contexts.<name>``)."""
    current_scope().contexts[name] = dict(value)


def track(name: str, props: Optional[Mapping[str, Any]] = None, **fields: Any) -> None:
    """Track a custom analytics event. ``props``: flat str/number/bool values, at most 64."""
    c = _client
    if c is not None:
        c.track(name, props, **fields)


def flush(timeout: float = 2.0) -> bool:
    """Send everything queued, waiting at most ``timeout`` seconds."""
    c = _client
    return c.flush(timeout) if c else True


def shutdown(timeout: float = 5.0) -> None:
    """End the session, flush and stop the SDK (also runs automatically at exit)."""
    global _client
    with _lock:
        c, _client = _client, None
    if c is not None:
        c.close(timeout)


def check_in(
    slug: str,
    status: str = "ok",
    *,
    check_in_id: Optional[str] = None,
    duration_ms: Optional[int] = None,
    message: Optional[str] = None,
    monitor: Optional[Mapping[str, Any]] = None,
    timeout: float = 10.0,
) -> bool:
    """Report a heartbeat check-in for the monitor ``slug``: ``in_progress``, ``ok`` or ``error``.

    ``monitor`` (see ``vaka.monitors.monitor_config``) creates the monitor on its first
    check-in. Never raises: returns False (and logs a warning) when the SDK is not initialized
    or the server refuses the check-in.
    """
    c = _client
    if c is None:
        return False
    if status not in _monitors.STATUSES:
        raise ValueError(f"status must be one of {', '.join(_monitors.STATUSES)}")
    body: Dict[str, Any] = {"status": status, "environment": c.options.environment}
    if c.options.release:
        body["release"] = c.options.release
    if check_in_id:
        body["check_in_id"] = check_in_id
    if duration_ms is not None:
        body["duration_ms"] = int(duration_ms)
    if message:
        body["message"] = message[:500]
    if monitor:
        body["monitor"] = dict(monitor)
    return _monitors.send(c.options.endpoint, c.options.key, slug, body, timeout=timeout)


def monitor(fn: Any = None, *, schedule: Any = None, every: Any = None, timezone: Optional[str] = None,
            grace_s: Optional[int] = None, max_runtime_s: Optional[int] = None, name: Optional[str] = None) -> Any:
    """Decorator for jobs.

    ``@vaka.monitor`` reports exceptions escaping the function (re-raised) inside an isolation
    scope. ``@vaka.monitor("nightly-backup", schedule="0 3 * * *")`` (or ``with
    vaka.monitor("nightly-backup"):``) also reports each run to a heartbeat monitor: Vaka alerts
    when a run is missed, fails or exceeds ``max_runtime_s``. With a schedule the monitor is
    created on first use.
    """
    if isinstance(fn, str):
        return _monitors.Monitor(fn, _monitors.monitor_config(schedule, every=every, timezone=timezone, grace_s=grace_s, max_runtime_s=max_runtime_s, name=name))
    if fn is None:
        return _report_exceptions
    return _report_exceptions(fn)


def _report_exceptions(fn: F) -> F:
    """Report exceptions escaping ``fn`` (re-raised) inside an isolation scope; also flushes."""
    import functools
    import inspect

    if inspect.iscoroutinefunction(fn):

        @functools.wraps(fn)
        async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
            with isolation_scope():
                try:
                    return await fn(*args, **kwargs)
                except Exception as e:
                    capture_exception(e, handled=False, tags={"mechanism": "monitor", "function": fn.__qualname__})
                    flush()
                    raise

        return async_wrapper  # type: ignore[return-value]

    @functools.wraps(fn)
    def wrapper(*args: Any, **kwargs: Any) -> Any:
        with isolation_scope():
            try:
                return fn(*args, **kwargs)
            except Exception as e:
                capture_exception(e, handled=False, tags={"mechanism": "monitor", "function": fn.__qualname__})
                flush()
                raise

    return wrapper  # type: ignore[return-value]


# ----- feature flags ---------------------------------------------------------------------


def load_flags(
    properties: Optional[Mapping[str, Any]] = None,
    distinct_id: Optional[str] = None,
    user_id: Optional[str] = None,
    timeout: float = 3.0,
) -> _flags.Flags:
    """Evaluate all flags for this process (or ``distinct_id``) and cache them for ``get_flag``."""
    c = _client
    if c is None:
        return _flags.Flags()
    props = dict(c.options.flag_properties)
    props.update(properties or {})
    result = _flags.evaluate(
        c.options.endpoint,
        c.options.key,
        distinct_id or c.distinct_id,
        user_id=user_id or current_scope().user_id,
        environment=c.options.environment,
        properties=props,
        timeout=timeout,
    )
    c.flags = result
    return result


def _load_flags_quietly() -> None:
    try:
        load_flags()
    except Exception as e:
        c = _client
        if c is not None:
            c.log(f"loading flags failed: {e}")


def evaluate_flags(
    distinct_id: str,
    user_id: Optional[str] = None,
    properties: Optional[Mapping[str, Any]] = None,
    timeout: float = 3.0,
) -> _flags.Flags:
    """Evaluate flags for one person (e.g. per request on a server). Nothing is cached.

    Values read with ``.get()`` are recorded on the current (request) scope.
    """
    c = _client
    if c is None:
        return _flags.Flags()
    props = dict(c.options.flag_properties)
    props.update(properties or {})
    return _flags.evaluate(
        c.options.endpoint, c.options.key, distinct_id, user_id=user_id,
        environment=c.options.environment, properties=props, timeout=timeout,
    )


def get_flag(key: str, default: Any = None) -> Any:
    """A flag value from the last ``load_flags()`` (recorded as an exposure)."""
    c = _client
    if c is None or c.flags is None:
        if default is not None:
            _flags.expose(key, default)
        return default
    return c.flags.get(key, default)


def is_enabled(key: str, default: bool = False) -> bool:
    c = _client
    if c is None or c.flags is None:
        return default
    return c.flags.is_enabled(key, default)


__all__ = [
    "init", "get_client", "is_initialized", "capture_exception", "capture_message", "capture_feedback", "add_breadcrumb",
    "set_user", "identify", "set_tag", "set_tags", "set_context", "track", "flush", "shutdown",
    "monitor", "load_flags", "evaluate_flags", "get_flag", "is_enabled", "isolation_scope", "Scope",
]
