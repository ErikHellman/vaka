"""Heartbeat monitors (``POST /ingest/v1/monitors/{slug}/check-ins``).

Cron jobs and scheduled tasks report each run so Vaka can alert when a run is missed, fails or
hangs. See https://vaka.hellsoft.se/docs/monitoring
"""

from __future__ import annotations

import json
import logging
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from typing import Any, Dict, Mapping, Optional, Union

from ._version import __version__

logger = logging.getLogger("vaka")

#: Error tag that links errors captured during a monitored run to that run.
CHECK_IN_TAG = "monitor.check_in_id"
STATUSES = ("in_progress", "ok", "error")


def monitor_config(
    schedule: Union[str, Mapping[str, Any], None] = None,
    *,
    every: Union[str, int, None] = None,
    timezone: Optional[str] = None,
    grace_s: Optional[int] = None,
    max_runtime_s: Optional[int] = None,
    name: Optional[str] = None,
) -> Optional[Dict[str, Any]]:
    """Build the ``monitor`` object that creates a monitor on its first check-in.

    ``schedule`` is a cron expression (``"0 3 * * *"``) or a full schedule mapping; ``every`` is
    an interval (``"15m"``). Returns None when neither is given.
    """
    if isinstance(schedule, Mapping):
        sched: Optional[Dict[str, Any]] = dict(schedule)
    elif schedule:
        sched = {"cron": schedule}
        if timezone:
            sched["timezone"] = timezone
    elif every is not None:
        sched = {"every": every}
    else:
        return None
    out: Dict[str, Any] = {"schedule": sched}
    if grace_s is not None:
        out["grace_s"] = grace_s
    if max_runtime_s is not None:
        out["max_runtime_s"] = max_runtime_s
    if name:
        out["name"] = name
    return out


def send(endpoint: str, key: str, slug: str, body: Mapping[str, Any], timeout: float = 10.0) -> bool:
    url = endpoint.rstrip("/") + "/ingest/v1/monitors/" + urllib.parse.quote(slug, safe="") + "/check-ins"
    data = json.dumps(body, default=str).encode("utf-8")
    headers = {"X-Vaka-Key": key, "User-Agent": f"vaka-python/{__version__}", "Content-Type": "application/json"}
    req = urllib.request.Request(url, data=data, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 (URL is configured)
            return 200 <= resp.status < 300
    except urllib.error.HTTPError as e:
        detail = e.read(300).decode("utf-8", "replace") if e.fp else ""
        logger.warning("vaka: check-in for %s failed: HTTP %s %s", slug, e.code, detail)
    except Exception as e:  # noqa: BLE001 (a check-in must never break the job)
        logger.warning("vaka: check-in for %s failed: %s", slug, e)
    return False


class Monitor:
    """A monitored run, usable as a decorator or a (sync or async) context manager.

    Reports ``in_progress`` on entry, then ``ok`` with the duration, or ``error`` (capturing the
    exception, tagged with the run) on an exception, which is re-raised.
    """

    def __init__(self, slug: str, config: Optional[Dict[str, Any]] = None):
        self.slug = slug
        self.config = config
        self._runs: list = []

    # -- context manager ------------------------------------------------------------------
    def _start(self) -> None:
        from . import _api
        from ._client import isolation_scope

        check_in_id = str(uuid.uuid4())
        _api.check_in(self.slug, "in_progress", check_in_id=check_in_id, monitor=self.config)
        cm = isolation_scope()
        scope = cm.__enter__()
        scope.tags[CHECK_IN_TAG] = check_in_id
        scope.tags["monitor.slug"] = self.slug
        self._runs.append((check_in_id, time.monotonic(), cm))

    def _finish(self, exc: Optional[BaseException]) -> None:
        from . import _api

        check_in_id, started, cm = self._runs.pop()
        duration_ms = int((time.monotonic() - started) * 1000)
        try:
            if exc is None:
                _api.check_in(self.slug, "ok", check_in_id=check_in_id, duration_ms=duration_ms)
            else:
                if isinstance(exc, Exception):
                    _api.capture_exception(exc, handled=False, tags={"mechanism": "monitor"})
                    _api.flush()
                _api.check_in(self.slug, "error", check_in_id=check_in_id, duration_ms=duration_ms, message=str(exc) or type(exc).__name__)
        finally:
            cm.__exit__(None, None, None)

    def __enter__(self) -> "Monitor":
        self._start()
        return self

    def __exit__(self, exc_type: Any, exc: Optional[BaseException], tb: Any) -> bool:
        self._finish(exc)
        return False

    async def __aenter__(self) -> "Monitor":
        self._start()
        return self

    async def __aexit__(self, exc_type: Any, exc: Optional[BaseException], tb: Any) -> bool:
        self._finish(exc)
        return False

    # -- decorator ------------------------------------------------------------------------
    def __call__(self, fn: Any) -> Any:
        import functools
        import inspect

        if inspect.iscoroutinefunction(fn):

            @functools.wraps(fn)
            async def async_wrapper(*args: Any, **kwargs: Any) -> Any:
                async with Monitor(self.slug, self.config):
                    return await fn(*args, **kwargs)

            return async_wrapper

        @functools.wraps(fn)
        def wrapper(*args: Any, **kwargs: Any) -> Any:
            with Monitor(self.slug, self.config):
                return fn(*args, **kwargs)

        return wrapper
