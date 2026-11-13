"""Background batching transport for the Vaka ingest API (``POST /ingest/v1/batch``)."""

from __future__ import annotations

import gzip
import json
import threading
import time
import urllib.error
import urllib.request
from collections import deque
from typing import Any, Deque, Dict, List, Tuple

from ._version import __version__

SDK_NAME = "vaka-python"
MAX_BATCH = 1000
MAX_QUEUE = 1000
MIN_BACKOFF = 5.0
MAX_BACKOFF = 300.0


def post_json(
    url: str, key: str, body: Any, timeout: float = 10.0, debug: bool = False
) -> Tuple[int, str]:
    """POST a JSON body (gzip-compressed when large). Returns (status, response text)."""
    data = json.dumps(body, default=str, separators=(",", ":")).encode("utf-8")
    headers = {
        "Content-Type": "application/json",
        "X-Vaka-Key": key,
        "User-Agent": f"{SDK_NAME}/{__version__}",
    }
    if len(data) > 4096:
        data = gzip.compress(data)
        headers["Content-Encoding"] = "gzip"
    req = urllib.request.Request(url, data=data, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 (URL is configured)
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        text = e.read().decode("utf-8", "replace") if e.fp else ""
        return e.code, text


class Transport:
    """Queues items and sends them in batches from a daemon thread.

    Errors are sent right away; everything else at most every ``flush_interval`` seconds or
    when 50 items are queued. 429/5xx and network errors back off exponentially (5 s to 5 min),
    401 stops sending until the process restarts, 413 splits the batch.
    """

    def __init__(self, endpoint: str, key: str, flush_interval: float = 5.0, debug: bool = False):
        self.url = endpoint.rstrip("/") + "/ingest/v1/batch"
        self.key = key
        self.flush_interval = flush_interval
        self.debug = debug
        self._queue: Deque[Dict[str, Any]] = deque(maxlen=MAX_QUEUE)
        self._cond = threading.Condition()
        self._urgent = False
        self._flush_waiters: List[threading.Event] = []
        self._stopped = False
        self._disabled = False
        self._backoff = 0.0
        self._send_lock = threading.Lock()
        self._thread = threading.Thread(target=self._run, name="vaka-sdk", daemon=True)
        self._thread.start()

    def log(self, msg: str) -> None:
        if self.debug:
            import sys

            print(f"[vaka] {msg}", file=sys.stderr)

    def send(self, item: Dict[str, Any]) -> None:
        if self._disabled or self._stopped:
            return
        with self._cond:
            self._queue.append(item)
            if item.get("type") == "error" or len(self._queue) >= 50:
                self._urgent = True
                self._cond.notify()

    def flush(self, timeout: float = 2.0) -> bool:
        """Send everything queued; wait up to ``timeout`` seconds. Returns True when done."""
        if not self._thread.is_alive():
            self._drain()
            return True
        done = threading.Event()
        with self._cond:
            self._flush_waiters.append(done)
            self._cond.notify()
        return done.wait(timeout)

    def send_now(self, item: Dict[str, Any], timeout: float = 2.0) -> None:
        """Queue ``item`` and flush synchronously from the calling thread.

        Used by the crash hooks: the interpreter may exit right after, so the background thread
        cannot be relied on.
        """
        with self._cond:
            self._queue.append(item)
        deadline = time.monotonic() + timeout
        if self._send_lock.acquire(timeout=timeout):
            try:
                self._drain_locked(deadline)
            finally:
                self._send_lock.release()

    def close(self, timeout: float = 5.0) -> None:
        if self._stopped:
            return
        self.flush(timeout)
        with self._cond:
            self._stopped = True
            self._cond.notify()

    def _run(self) -> None:
        last = time.monotonic()
        while True:
            with self._cond:
                waiters: List[threading.Event] = []
                while True:
                    if self._stopped:
                        return
                    if self._flush_waiters:
                        waiters, self._flush_waiters = self._flush_waiters, []
                        break
                    due = last + self.flush_interval + self._backoff
                    now = time.monotonic()
                    if self._urgent and self._backoff == 0:
                        break
                    if now >= due:
                        break
                    self._cond.wait(due - now)
                self._urgent = False
            self._drain()
            last = time.monotonic()
            for w in waiters:
                w.set()

    def _drain(self) -> None:
        with self._send_lock:
            self._drain_locked(time.monotonic() + 30)

    def _take(self) -> List[Dict[str, Any]]:
        with self._cond:
            items = list(self._queue)[:MAX_BATCH]
            for _ in items:
                self._queue.popleft()
            return items

    def _requeue(self, items: List[Dict[str, Any]]) -> None:
        with self._cond:
            for it in reversed(items):
                if len(self._queue) >= MAX_QUEUE:
                    break
                self._queue.appendleft(it)

    def _drain_locked(self, deadline: float) -> None:
        while not self._disabled and time.monotonic() < deadline:
            items = self._take()
            if not items:
                return
            if not self._post(items):
                return

    def _post(self, items: List[Dict[str, Any]]) -> bool:
        """Send one batch. Returns False when sending should pause (error or backoff)."""
        body = {"sdk": {"name": SDK_NAME, "version": __version__}, "items": items}
        try:
            status, text = post_json(self.url, self.key, body)
        except Exception as e:  # network error, DNS, TLS, timeout
            self.log(f"send failed: {e}")
            self._requeue(items)
            self._backoff = min(max(self._backoff * 2, MIN_BACKOFF), MAX_BACKOFF)
            return False
        if status == 429 or status >= 500:
            self.log(f"ingest returned {status}; backing off")
            self._requeue(items)
            self._backoff = min(max(self._backoff * 2, MIN_BACKOFF), MAX_BACKOFF)
            return False
        self._backoff = 0.0
        if status == 401:
            self.log("ingest key rejected (401); SDK disabled until restart")
            self._disabled = True
            with self._cond:
                self._queue.clear()
            return False
        if status == 413 and len(items) > 1:
            half = len(items) // 2
            return self._post(items[:half]) and self._post(items[half:])
        if status >= 400:
            self.log(f"ingest returned {status}: {text}")
        else:
            self.log(f"sent {len(items)} items ({status}) {text}")
        return True


__all__ = ["Transport", "post_json", "SDK_NAME"]
