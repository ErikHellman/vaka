"""User feedback (``POST /ingest/v1/feedback``).

Backend services forward what their users wrote (a support form, in-app chat) with
:func:`vaka.capture_feedback`. See https://vaka.hellsoft.se/docs/feedback
"""

from __future__ import annotations

import json
import mimetypes
import urllib.error
import urllib.request
import uuid
from typing import Any, Dict, Mapping, NamedTuple, Optional, Sequence, Tuple, Union

from ._version import __version__

MAX_MESSAGE_CHARS = 5000
MAX_ATTACHMENTS = 3

#: ``(filename, data)`` or ``(filename, data, content_type)``; PNG, JPEG or WebP, 5 MB each.
Attachment = Union[Tuple[str, bytes], Tuple[str, bytes, str]]


class FeedbackResult(NamedTuple):
    id: str
    #: e.g. ``API-F12``
    short_id: str


class FeedbackError(Exception):
    """The feedback was not stored. ``status`` is the HTTP status, or 0 when nothing was sent
    (429 means a rate limit was hit)."""

    def __init__(self, message: str, status: int = 0):
        super().__init__(message)
        self.status = status


def build_item(
    message: str,
    common: Mapping[str, Any],
    *,
    email: Optional[str] = None,
    name: Optional[str] = None,
    event_id: Optional[str] = None,
    issue: Optional[str] = None,
    user_id: Optional[str] = None,
    session_id: Optional[str] = None,
    url: Optional[str] = None,
    tags: Optional[Mapping[str, Any]] = None,
) -> Dict[str, Any]:
    text = (message or "").strip()
    if not text:
        raise FeedbackError("feedback needs a message")
    item: Dict[str, Any] = dict(common)
    item.update({"id": str(uuid.uuid4()), "source": "api", "message": text[:MAX_MESSAGE_CHARS]})
    for k, v in (
        ("email", (email or "").strip()),
        ("name", (name or "").strip()),
        ("event_id", event_id),
        ("issue", issue),
        ("user_id", user_id),
        ("session_id", session_id),
        ("url", url),
    ):
        if v:
            item[k] = v
    merged = dict(item.get("tags") or {})
    merged.update({k: str(v) for k, v in (tags or {}).items()})
    if merged:
        item["tags"] = merged
    return item


def _multipart(item: Mapping[str, Any], files: Sequence[Attachment]) -> Tuple[bytes, str]:
    boundary = "vaka-" + uuid.uuid4().hex
    out = bytearray()
    out += f'--{boundary}\r\nContent-Disposition: form-data; name="feedback"\r\nContent-Type: application/json\r\n\r\n'.encode()
    out += json.dumps(item, default=str).encode("utf-8") + b"\r\n"
    for f in files:
        filename, data = f[0], f[1]
        ctype = f[2] if len(f) > 2 else (mimetypes.guess_type(filename)[0] or "image/jpeg")  # type: ignore[misc]
        safe = filename.replace('"', "").replace("\r", "").replace("\n", "")
        out += f'--{boundary}\r\nContent-Disposition: form-data; name="attachment"; filename="{safe}"\r\nContent-Type: {ctype}\r\n\r\n'.encode()
        out += bytes(data) + b"\r\n"
    out += f"--{boundary}--\r\n".encode()
    return bytes(out), f"multipart/form-data; boundary={boundary}"


def send(
    endpoint: str,
    key: str,
    item: Mapping[str, Any],
    attachments: Optional[Sequence[Attachment]] = None,
    ip: Optional[str] = None,
    timeout: float = 15.0,
) -> FeedbackResult:
    files = list(attachments or [])
    if len(files) > MAX_ATTACHMENTS:
        raise FeedbackError(f"at most {MAX_ATTACHMENTS} attachments per feedback")
    headers = {"X-Vaka-Key": key, "User-Agent": f"vaka-python/{__version__}"}
    if ip:
        headers["X-Forwarded-For"] = ip
    if files:
        data, headers["Content-Type"] = _multipart(item, files)
    else:
        data = json.dumps(item, default=str).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(endpoint.rstrip("/") + "/ingest/v1/feedback", data=data, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:  # noqa: S310 (URL is configured)
            status, text = resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        status, text = e.code, (e.read().decode("utf-8", "replace") if e.fp else "")
    except (urllib.error.URLError, OSError) as e:
        raise FeedbackError(f"vaka feedback: {e}") from e
    try:
        body = json.loads(text) if text else {}
    except ValueError:
        body = {}
    if not 200 <= status < 300:
        detail = f": {body['error']}" if isinstance(body, dict) and body.get("error") else ""
        raise FeedbackError(f"vaka feedback: HTTP {status}{detail}", status)
    return FeedbackResult(str(body.get("id") or item["id"]), str(body.get("short_id") or ""))
