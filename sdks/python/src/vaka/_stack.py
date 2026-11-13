"""Convert Python exceptions and tracebacks into Vaka's exception/frame format."""

from __future__ import annotations

import linecache
import os
import sys
import sysconfig
import traceback
from types import FrameType, TracebackType
from typing import Any, Dict, Iterable, List, Optional, Sequence

MAX_FRAMES = 100
MAX_CHAIN = 10
CONTEXT_LINES = 5


def _library_roots() -> List[str]:
    roots = set()
    for name in ("stdlib", "platstdlib", "purelib", "platlib"):
        try:
            p = sysconfig.get_path(name)
        except KeyError:
            p = None
        if p:
            roots.add(os.path.realpath(p))
    for p in sys.path:
        if p and ("site-packages" in p or "dist-packages" in p):
            roots.add(os.path.realpath(p))
    roots.add(os.path.realpath(os.path.dirname(os.__file__)))
    return sorted(roots, key=len, reverse=True)


_LIBRARY_ROOTS = _library_roots()
_SDK_ROOT = os.path.dirname(os.path.abspath(__file__))


class InApp:
    """Decides whether a frame is the app's own code."""

    def __init__(self, include: Sequence[str] = (), exclude: Sequence[str] = ()):
        self.include = tuple(include)
        self.exclude = tuple(exclude)

    def __call__(self, module: Optional[str], abs_path: Optional[str]) -> bool:
        if module:
            if self.include and any(module == p or module.startswith(p + ".") for p in self.include):
                return True
            if self.exclude and any(module == p or module.startswith(p + ".") for p in self.exclude):
                return False
        if not abs_path or abs_path.startswith("<"):
            return False
        real = os.path.realpath(abs_path)
        if "site-packages" in real or "dist-packages" in real:
            return False
        if any(real.startswith(r + os.sep) for r in _LIBRARY_ROOTS):
            return False
        return not self.include


def _relative(abs_path: str) -> str:
    """Repo-relative-looking path for in-app files (so source links work), basename otherwise."""
    try:
        rel = os.path.relpath(abs_path)
    except ValueError:  # different drive on Windows
        return abs_path
    if rel.startswith(".."):
        for marker in ("site-packages" + os.sep, "dist-packages" + os.sep):
            if marker in abs_path:
                return abs_path.split(marker, 1)[1]
        return abs_path
    return rel


def frame_dict(
    frame: FrameType, lineno: Optional[int], in_app: InApp, with_context: bool = True
) -> Dict[str, Any]:
    code = frame.f_code
    abs_path = code.co_filename
    module = frame.f_globals.get("__name__") if frame.f_globals else None
    fn = getattr(code, "co_qualname", None) or code.co_name
    d: Dict[str, Any] = {
        "function": fn,
        "module": module,
        "filename": _relative(abs_path) if not abs_path.startswith("<") else abs_path,
        "abs_path": abs_path,
        "lineno": lineno,
        "in_app": in_app(module, abs_path),
    }
    if with_context and lineno and not abs_path.startswith("<"):
        lines = linecache.getlines(abs_path, frame.f_globals)
        if lines and 0 < lineno <= len(lines):
            i = lineno - 1
            d["context_line"] = lines[i].rstrip("\r\n")
            d["pre_context"] = [line.rstrip("\r\n") for line in lines[max(0, i - CONTEXT_LINES) : i]]
            d["post_context"] = [line.rstrip("\r\n") for line in lines[i + 1 : i + 1 + CONTEXT_LINES]]
    return d


def frames_from_traceback(tb: Optional[TracebackType], in_app: InApp) -> List[Dict[str, Any]]:
    """Frames ordered crash site first, SDK frames dropped."""
    out: List[Dict[str, Any]] = []
    for frame, lineno in traceback.walk_tb(tb):
        out.append(frame_dict(frame, lineno, in_app))
    out.reverse()
    out = [f for f in out if not _is_sdk(f)]
    return out[:MAX_FRAMES]


def current_frames(in_app: InApp, skip: int = 0) -> List[Dict[str, Any]]:
    """The calling stack, crash site (the caller) first, SDK frames dropped."""
    out: List[Dict[str, Any]] = []
    for frame, lineno in traceback.walk_stack(sys._getframe(skip + 1)):
        f = frame_dict(frame, lineno, in_app)
        if _is_sdk(f):
            continue
        out.append(f)
        if len(out) >= MAX_FRAMES:
            break
    return out


def _is_sdk(f: Dict[str, Any]) -> bool:
    p = f.get("abs_path") or ""
    return p.startswith(_SDK_ROOT + os.sep) and "tests" not in p


def type_name(exc: BaseException) -> str:
    t = type(exc)
    mod = t.__module__
    if mod in ("builtins", "__main__", None):
        return t.__qualname__
    return f"{mod}.{t.__qualname__}"


def exception_chain(exc: BaseException) -> Iterable[BaseException]:
    """The exception, then its cause/context chain (outermost first)."""
    seen = set()
    cur: Optional[BaseException] = exc
    n = 0
    while cur is not None and id(cur) not in seen and n < MAX_CHAIN:
        seen.add(id(cur))
        yield cur
        n += 1
        if cur.__cause__ is not None:
            cur = cur.__cause__
        elif cur.__context__ is not None and not cur.__suppress_context__:
            cur = cur.__context__
        else:
            cur = None


def exceptions_payload(
    exc: BaseException, in_app: InApp, mechanism: Optional[str] = None, handled: bool = True
) -> List[Dict[str, Any]]:
    out = []
    for e in exception_chain(exc):
        frames = frames_from_traceback(e.__traceback__, in_app)
        item: Dict[str, Any] = {
            "type": type_name(e),
            "value": _safe_str(e),
            "module": type(e).__module__,
            "stacktrace": frames,
        }
        if mechanism:
            item["mechanism"] = {"type": mechanism, "handled": handled}
        out.append(item)
        # Grouped exceptions (3.11+): include the first few sub-exceptions.
        subs = getattr(e, "exceptions", None)
        if isinstance(subs, tuple) and isinstance(e, BaseException) and type(e).__name__.endswith("ExceptionGroup"):
            for sub in subs[:3]:
                if isinstance(sub, BaseException):
                    out.append(
                        {
                            "type": type_name(sub),
                            "value": _safe_str(sub),
                            "module": type(sub).__module__,
                            "stacktrace": frames_from_traceback(sub.__traceback__, in_app),
                        }
                    )
    return out


def _safe_str(e: BaseException) -> str:
    try:
        return str(e)
    except Exception:  # pragma: no cover - broken __str__
        return f"<unprintable {type(e).__name__}>"
