"""Standard-library ``logging`` integration.

Records at ``breadcrumb_level`` (INFO) and above become breadcrumbs; records at ``event_level``
(ERROR) and above are reported as Vaka errors, with the exception when ``exc_info`` is set
(``logger.exception(...)``). Installed by ``vaka.init()`` unless ``logging=False``.
"""

from __future__ import annotations

import logging
from typing import Any, Dict, Optional

_IGNORED_LOGGERS = {"vaka", "urllib3.connectionpool", "opentelemetry"}
_LEVELS = {
    logging.CRITICAL: "fatal",
    logging.ERROR: "error",
    logging.WARNING: "warning",
    logging.INFO: "info",
    logging.DEBUG: "debug",
}
_STD_ATTRS = set(vars(logging.LogRecord("", 0, "", 0, "", (), None))) | {"message", "asctime"}


def _level_name(levelno: int) -> str:
    for n in (logging.CRITICAL, logging.ERROR, logging.WARNING, logging.INFO):
        if levelno >= n:
            return _LEVELS[n]
    return "debug"


class VakaHandler(logging.Handler):
    """A logging handler that feeds Vaka breadcrumbs and errors."""

    def __init__(self, breadcrumb_level: int = logging.INFO, event_level: Optional[int] = logging.ERROR):
        super().__init__(level=min(breadcrumb_level, event_level or breadcrumb_level))
        self.breadcrumb_level = breadcrumb_level
        self.event_level = event_level

    def emit(self, record: logging.LogRecord) -> None:
        if record.name.split(".")[0] in _IGNORED_LOGGERS or record.name in _IGNORED_LOGGERS:
            return
        if getattr(record, "vaka_skip", False):
            return
        from .. import _api

        try:
            message = record.getMessage()
        except Exception:
            message = str(record.msg)
        level = _level_name(record.levelno)
        extra: Dict[str, Any] = {k: v for k, v in vars(record).items() if k not in _STD_ATTRS and not k.startswith("_")}
        if self.event_level is not None and record.levelno >= self.event_level:
            exc = record.exc_info[1] if record.exc_info else None
            tags = {"logger": record.name}
            contexts = {"log": {"logger": record.name, "message": message, "extra": _jsonable(extra)}} if extra else None
            if exc is not None:
                event_id = _api.capture_exception(exc, level=level, tags=tags, contexts=contexts, mechanism="logging")
            else:
                event_id = _api.capture_message(message, level=level, tags=tags, contexts=contexts)
            if event_id:
                # Exported over OTLP too (vaka.otel), the record must not become a second error.
                setattr(record, "vaka.error_captured", True)
        # Add the breadcrumb after capturing so an error isn't its own breadcrumb.
        if record.levelno >= self.breadcrumb_level:
            data = {"logger": record.name}
            if extra:
                data.update(_jsonable(extra))
            _api.add_breadcrumb(message, category="console", level=level, data=data)


def _jsonable(d: Dict[str, Any]) -> Dict[str, Any]:
    out = {}
    for k, v in d.items():
        out[k] = v if isinstance(v, (str, int, float, bool, type(None))) else repr(v)
    return out


_original_call_handlers = logging.Logger.callHandlers
_active: Optional[VakaHandler] = None


def _call_handlers(self: logging.Logger, record: logging.LogRecord) -> None:
    h = _active
    if h is not None and record.levelno >= h.level:
        try:
            h.handle(record)
        except Exception:  # never break the app's logging
            pass
    _original_call_handlers(self, record)


def install(breadcrumb_level: int = logging.INFO, event_level: Optional[int] = logging.ERROR) -> VakaHandler:
    """Hook into ``Logger.callHandlers`` instead of adding a root handler.

    The app's handlers, levels and the ``lastResort`` fallback keep working exactly as before.
    Records reach Vaka when the logger's level lets them through (the root logger defaults to
    WARNING; set INFO to get INFO breadcrumbs).
    """
    global _active
    handler = VakaHandler(breadcrumb_level, event_level)
    _active = handler
    logging.Logger.callHandlers = _call_handlers  # type: ignore[method-assign]
    return handler


def uninstall(handler: logging.Handler) -> None:
    global _active
    if _active is handler:
        _active = None
        logging.Logger.callHandlers = _original_call_handlers  # type: ignore[method-assign]
