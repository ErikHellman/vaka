"""The Vaka client: scope, sessions, errors, events and crash hooks."""

from __future__ import annotations

import atexit
import contextvars
import copy
import os
import platform
import socket
import sys
import threading
import time
import uuid
from contextlib import contextmanager
from typing import Any, Callable, Dict, Iterator, List, Mapping, Optional, Sequence

from . import _stack
from ._transport import Transport

MAX_BREADCRUMBS = 100
LEVELS = ("fatal", "error", "warning", "info", "debug")


def now_ms() -> int:
    return int(time.time() * 1000)


def new_id() -> str:
    return str(uuid.uuid4())


def _norm_level(level: str) -> str:
    level = (level or "error").lower()
    return {"warn": "warning", "critical": "fatal", "exception": "error"}.get(level, level)


class Scope:
    """Data attached to everything captured while the scope is active."""

    def __init__(self) -> None:
        self.user_id: Optional[str] = None
        self.tags: Dict[str, str] = {}
        self.contexts: Dict[str, Any] = {}
        self.breadcrumbs: List[Dict[str, Any]] = []
        self.url: Optional[str] = None
        self.transaction: Optional[str] = None
        self.trace_id: Optional[str] = None
        self.flags: Dict[str, Any] = {}

    def copy(self) -> "Scope":
        s = Scope()
        s.user_id = self.user_id
        s.tags = dict(self.tags)
        s.contexts = copy.deepcopy(self.contexts)
        s.breadcrumbs = list(self.breadcrumbs)
        s.url = self.url
        s.transaction = self.transaction
        s.trace_id = self.trace_id
        s.flags = dict(self.flags)
        return s


_global_scope = Scope()
_scope_lock = threading.Lock()
_current_scope: contextvars.ContextVar[Optional[Scope]] = contextvars.ContextVar("vaka_scope", default=None)


def current_scope() -> Scope:
    return _current_scope.get() or _global_scope


@contextmanager
def isolation_scope() -> Iterator[Scope]:
    """Run a block (a request, a job) with its own copy of the scope.

    Breadcrumbs, tags and the user set inside do not leak into other requests. Framework
    integrations do this per request; use it yourself for background jobs.
    """
    with _scope_lock:
        scope = current_scope().copy()
    token = _current_scope.set(scope)
    try:
        yield scope
    finally:
        _current_scope.reset(token)


class Options:
    def __init__(
        self,
        key: Optional[str] = None,
        endpoint: Optional[str] = None,
        release: Optional[str] = None,
        environment: Optional[str] = None,
        service: Optional[str] = None,
        capture_unhandled: bool = True,
        sessions: bool = True,
        logging: bool = True,
        breadcrumb_level: int = 20,
        event_level: Optional[int] = 40,
        in_app_include: Sequence[str] = (),
        in_app_exclude: Sequence[str] = (),
        flush_interval: float = 5.0,
        before_send: Optional[Callable[[Dict[str, Any]], Optional[Dict[str, Any]]]] = None,
        sample_rate: float = 1.0,
        flags: bool = False,
        flag_properties: Optional[Mapping[str, Any]] = None,
        distinct_id: Optional[str] = None,
        debug: bool = False,
    ) -> None:
        env = os.environ
        self.key = key if key is not None else env.get("VAKA_KEY", "")
        self.endpoint = (endpoint if endpoint is not None else env.get("VAKA_ENDPOINT", "")).rstrip("/")
        self.release = release if release is not None else env.get("VAKA_RELEASE")
        self.environment = environment or env.get("VAKA_ENVIRONMENT") or "production"
        self.service = service or env.get("VAKA_SERVICE") or env.get("OTEL_SERVICE_NAME")
        self.capture_unhandled = capture_unhandled
        self.sessions = sessions
        self.logging = logging
        self.breadcrumb_level = breadcrumb_level
        self.event_level = event_level
        self.in_app_include = tuple(in_app_include)
        self.in_app_exclude = tuple(in_app_exclude)
        self.flush_interval = flush_interval
        self.before_send = before_send
        self.sample_rate = sample_rate
        self.flags = flags
        self.flag_properties = dict(flag_properties or {})
        self.distinct_id = distinct_id
        self.debug = debug or env.get("VAKA_DEBUG", "") in ("1", "true")


class Client:
    def __init__(self, options: Options):
        self.options = options
        self.in_app = _stack.InApp(options.in_app_include, options.in_app_exclude)
        self.transport = Transport(options.endpoint, options.key, options.flush_interval, options.debug)
        self.distinct_id = options.distinct_id or new_id()
        self.session_lock = threading.Lock()
        self.session: Optional[Dict[str, Any]] = None
        self.session_started = 0.0
        self.session_errors = 0
        self.flags: Any = None  # vaka.flags.Flags from load_flags()
        self.closed = False
        self._prev_excepthook: Optional[Callable[..., Any]] = None
        self._prev_threading_hook: Optional[Callable[..., Any]] = None
        self._log_handler: Any = None

    # ----- common fields ---------------------------------------------------------------

    def log(self, msg: str) -> None:
        self.transport.log(msg)

    def common(self, scope: Optional[Scope] = None) -> Dict[str, Any]:
        scope = scope or current_scope()
        m: Dict[str, Any] = {"platform": "python", "environment": self.options.environment}
        if self.options.release:
            m["release"] = self.options.release
        if scope.user_id:
            m["user_id"] = scope.user_id
        m["anon_id"] = self.distinct_id
        if self.session:
            m["session_id"] = self.session["session_id"]
        if scope.flags:
            m["flags"] = {k: _flag_str(v) for k, v in list(scope.flags.items())[:32]}
        return m

    def _send(self, item: Dict[str, Any], sync: bool = False) -> Optional[Dict[str, Any]]:
        if self.closed:
            return None
        if self.options.before_send is not None:
            try:
                result = self.options.before_send(item)
            except Exception as e:  # never let a user callback break the app
                self.log(f"before_send raised {e!r}; sending unchanged")
                result = item
            if result is None:
                return None
            item = result
        if sync:
            self.transport.send_now(item)
        else:
            self.transport.send(item)
        return item

    # ----- sessions --------------------------------------------------------------------

    def start_session(self) -> None:
        with self.session_lock:
            self.session = {"session_id": new_id(), "started": now_ms()}
            self.session_started = time.monotonic()
            self.session_errors = 0
        self._send(self._session_item("ok"))

    def _session_item(self, status: str) -> Dict[str, Any]:
        assert self.session is not None
        m = self.common(_global_scope)
        m.update(
            {
                "type": "session",
                "status": status,
                "started": self.session["started"],
                "timestamp": now_ms(),
                "duration_ms": int((time.monotonic() - self.session_started) * 1000),
                "errors": self.session_errors,
            }
        )
        return m

    def end_session(self, status: str = "exited", sync: bool = False) -> None:
        with self.session_lock:
            if self.session is None:
                return
            if status == "exited" and self.session_errors > 0:
                status = "errored"
            item = self._session_item(status)
            self.session = None
        self._send(item, sync=sync)

    def _note_error(self) -> None:
        with self.session_lock:
            self.session_errors += 1

    # ----- capture ---------------------------------------------------------------------

    def error_item(
        self,
        level: str,
        handled: bool,
        exceptions: List[Dict[str, Any]],
        message: Optional[str] = None,
        tags: Optional[Mapping[str, Any]] = None,
        contexts: Optional[Mapping[str, Any]] = None,
        fingerprint: Optional[Sequence[str]] = None,
        scope: Optional[Scope] = None,
    ) -> Dict[str, Any]:
        scope = scope or current_scope()
        m = self.common(scope)
        all_tags = dict(scope.tags)
        if self.options.service:
            all_tags.setdefault("service", self.options.service)
        if scope.transaction:
            all_tags.setdefault("transaction", scope.transaction)
        if tags:
            all_tags.update({k: str(v) for k, v in tags.items()})
        ctx: Dict[str, Any] = {
            "runtime": {
                "name": platform.python_implementation(),
                "version": platform.python_version(),
            },
            "os": {"name": platform.system(), "version": platform.release(), "arch": platform.machine()},
        }
        try:
            ctx["server"] = {"host": socket.gethostname()}
        except Exception:  # pragma: no cover
            pass
        ctx.update(copy.deepcopy(scope.contexts))
        if contexts:
            ctx.update(contexts)
        m.update(
            {
                "type": "error",
                "event_id": new_id(),
                "timestamp": now_ms(),
                "level": level,
                "handled": handled,
                "fatal": level == "fatal",
                "exceptions": exceptions,
                "breadcrumbs": list(scope.breadcrumbs[-MAX_BREADCRUMBS:]),
                "tags": all_tags,
                "contexts": ctx,
                "device": {"os_name": platform.system(), "os_version": platform.release()},
            }
        )
        if scope.user_id:
            m["user"] = {"id": scope.user_id}
        if message:
            m["message"] = message
        if scope.url:
            m["url"] = scope.url
        if fingerprint:
            m["fingerprint"] = list(fingerprint)
        trace_id = scope.trace_id or _otel_trace_id()
        if trace_id:
            m["trace_id"] = trace_id
        return m

    def _sampled(self) -> bool:
        if self.options.sample_rate >= 1.0:
            return True
        import random

        return random.random() < self.options.sample_rate

    def capture_exception(
        self,
        exc: Optional[BaseException] = None,
        level: str = "error",
        handled: bool = True,
        mechanism: Optional[str] = None,
        sync: bool = False,
        **kwargs: Any,
    ) -> Optional[str]:
        if exc is None:
            exc = sys.exc_info()[1]
            if exc is None:
                return None
        if getattr(exc, "__vaka_captured__", False):
            return None
        try:
            exc.__vaka_captured__ = True  # type: ignore[attr-defined]
        except Exception:
            pass
        level = _norm_level(level)
        if level in ("error", "fatal"):
            self._note_error()
        if not self._sampled():
            return None
        exceptions = _stack.exceptions_payload(exc, self.in_app)
        tags = dict(kwargs.pop("tags", None) or {})
        if mechanism:
            tags.setdefault("mechanism", mechanism)
        item = self.error_item(level, handled, exceptions, tags=tags, **kwargs)
        sent = self._send(item, sync=sync)
        if sent and "trace_id" in item:
            _mark_otel_span()
        return item["event_id"] if sent else None

    def capture_message(self, message: str, level: str = "info", **kwargs: Any) -> Optional[str]:
        level = _norm_level(level)
        if level in ("error", "fatal"):
            self._note_error()
        if not self._sampled():
            return None
        frames = _stack.current_frames(self.in_app, skip=2)
        exc = {"type": "Message", "value": message, "stacktrace": frames}
        item = self.error_item(level, True, [exc], message=message, **kwargs)
        sent = self._send(item)
        return item["event_id"] if sent else None

    def track(self, name: str, props: Optional[Mapping[str, Any]] = None, **kwargs: Any) -> None:
        m = self.common()
        m.update({"type": "event", "name": name, "timestamp": now_ms()})
        if props:
            m["props"] = {k: v if isinstance(v, (str, int, float, bool)) else str(v) for k, v in list(props.items())[:64]}
        scope = current_scope()
        url = kwargs.pop("url", None) or scope.url
        if url:
            m["url"] = url
        m.update(kwargs)
        self._send(m)

    # ----- hooks -----------------------------------------------------------------------

    def install_hooks(self) -> None:
        if self.options.capture_unhandled:
            self._prev_excepthook = sys.excepthook
            sys.excepthook = self._excepthook
            if hasattr(threading, "excepthook"):
                self._prev_threading_hook = threading.excepthook
                threading.excepthook = self._threading_excepthook
        if self.options.logging:
            from .integrations.logging import install

            self._log_handler = install(self.options.breadcrumb_level, self.options.event_level)
        atexit.register(self.close)

    def uninstall_hooks(self) -> None:
        if self._prev_excepthook is not None and sys.excepthook == self._excepthook:
            sys.excepthook = self._prev_excepthook
        if self._prev_threading_hook is not None and threading.excepthook == self._threading_excepthook:
            threading.excepthook = self._prev_threading_hook
        if self._log_handler is not None:
            from .integrations.logging import uninstall

            uninstall(self._log_handler)
            self._log_handler = None
        try:
            atexit.unregister(self.close)
        except Exception:  # pragma: no cover
            pass

    def capture_crash(self, exc: BaseException, mechanism: str) -> None:
        """Report an unhandled exception synchronously and mark the session crashed."""
        if isinstance(exc, (KeyboardInterrupt, SystemExit)):
            return
        try:
            self.capture_exception(exc, level="fatal", handled=False, mechanism=mechanism, sync=True)
            if mechanism == "excepthook":
                self.end_session("crashed", sync=True)
        except Exception as e:  # pragma: no cover
            self.log(f"failed to report crash: {e!r}")

    def _excepthook(self, exc_type: Any, exc: BaseException, tb: Any) -> None:
        if exc is not None:
            if exc.__traceback__ is None and tb is not None:
                exc = exc.with_traceback(tb)
            self.capture_crash(exc, "excepthook")
        hook = self._prev_excepthook or sys.__excepthook__
        hook(exc_type, exc, tb)

    def _threading_excepthook(self, args: Any) -> None:
        if args.exc_value is not None:
            self.capture_crash(args.exc_value, "threading")
        hook = self._prev_threading_hook or threading.__excepthook__
        hook(args)

    def flush(self, timeout: float = 2.0) -> bool:
        return self.transport.flush(timeout)

    def close(self, timeout: float = 5.0) -> None:
        if self.closed:
            return
        self.end_session("exited")
        self.transport.close(timeout)
        self.closed = True
        self.uninstall_hooks()


def _flag_str(v: Any) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    return str(v)


def _otel_trace_id() -> Optional[str]:
    """The active OpenTelemetry trace id, if opentelemetry-api is installed and a span is active."""
    mod = sys.modules.get("opentelemetry.trace")
    if mod is None:
        return None
    try:
        ctx = mod.get_current_span().get_span_context()
        if ctx is not None and ctx.is_valid:
            return format(ctx.trace_id, "032x")
    except Exception:  # pragma: no cover
        return None
    return None


def _mark_otel_span() -> None:
    """Mark the active OTel span so Vaka doesn't turn its exception event into a second error."""
    mod = sys.modules.get("opentelemetry.trace")
    if mod is None:
        return
    try:
        span = mod.get_current_span()
        if span.is_recording():
            span.set_attribute("vaka.error_captured", True)
    except Exception:  # pragma: no cover
        pass
