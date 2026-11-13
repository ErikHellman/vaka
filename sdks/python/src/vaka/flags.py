"""Feature flag evaluation with Vaka (``POST /ingest/v1/flags``).

Values the app reads with :meth:`Flags.get` are recorded as exposures on the current scope, so
errors, events and sessions carry ``flags`` and Vaka can split crash rates and conversions by
variant (see the server's flags docs).
"""

from __future__ import annotations

from typing import Any, Dict, Iterator, Mapping, Optional

from ._client import current_scope


class Flags(Mapping[str, Any]):
    """Evaluated flag values: ``True``/``False`` for on/off flags, a variant key otherwise."""

    def __init__(self, values: Optional[Mapping[str, Any]] = None, details: Optional[Mapping[str, Any]] = None):
        self._values: Dict[str, Any] = dict(values or {})
        self.details: Dict[str, Any] = dict(details or {})

    def __getitem__(self, key: str) -> Any:
        return self._values[key]

    def __iter__(self) -> Iterator[str]:
        return iter(self._values)

    def __len__(self) -> int:
        return len(self._values)

    def __repr__(self) -> str:
        return f"Flags({self._values!r})"

    def get(self, key: str, default: Any = None) -> Any:  # type: ignore[override]
        """The flag's value (``default`` if unknown), recorded as an exposure."""
        value = self._values.get(key, default)
        expose(key, value if value is not None else False)
        return value

    def is_enabled(self, key: str, default: bool = False) -> bool:
        """True when the flag is on (any variant other than ``False`` counts as on)."""
        value = self.get(key, default)
        return value is not False and value is not None and value != "false"

    def peek(self, key: str, default: Any = None) -> Any:
        """The value without recording an exposure."""
        return self._values.get(key, default)


def expose(key: str, value: Any) -> None:
    """Record that the app used ``value`` for flag ``key`` (attached to everything captured next)."""
    current_scope().flags[key] = value


def evaluate(
    endpoint: str,
    key: str,
    distinct_id: str,
    user_id: Optional[str] = None,
    environment: Optional[str] = None,
    properties: Optional[Mapping[str, Any]] = None,
    timeout: float = 3.0,
) -> Flags:
    from ._transport import post_json
    import json

    body: Dict[str, Any] = {"distinct_id": distinct_id}
    if user_id:
        body["user_id"] = user_id
    if environment:
        body["environment"] = environment
    if properties:
        body["properties"] = {k: v if isinstance(v, (str, int, float, bool)) else str(v) for k, v in properties.items()}
    status, text = post_json(endpoint.rstrip("/") + "/ingest/v1/flags", key, body, timeout=timeout)
    if status >= 400:
        raise RuntimeError(f"flag evaluation failed with {status}: {text}")
    data = json.loads(text or "{}")
    return Flags(data.get("flags") or {}, data.get("details") or {})
