"""OpenTelemetry setup that exports traces and logs to Vaka over OTLP/HTTP.

Needs the ``otel`` extra: ``pip install "vaka-sdk[otel]"``.

    import vaka, vaka.otel
    vaka.init(key="vk_...", endpoint="https://vaka.example.com", release="2.4.0", service="checkout")
    vaka.otel.setup()

After this, any OpenTelemetry instrumentation (``opentelemetry-instrumentation-*`` packages,
``tracer.start_as_current_span``) sends spans to Vaka's performance views, and log records go
to Vaka's log search, tagged with the same release and environment as errors.
"""

from __future__ import annotations

import logging
from typing import Any, Dict, Mapping, Optional, Tuple

from . import _api
from ._client import current_scope

ERROR_CAPTURED = "vaka.error_captured"


def _require() -> None:
    try:
        import opentelemetry.sdk  # noqa: F401
        import opentelemetry.exporter.otlp.proto.http  # noqa: F401
    except ImportError as e:  # pragma: no cover - depends on the environment
        raise ImportError(
            'vaka.otel needs the OpenTelemetry SDK: pip install "vaka-sdk[otel]"'
        ) from e


def resource_attributes(service_name: Optional[str] = None, extra: Optional[Mapping[str, Any]] = None) -> Dict[str, Any]:
    """OTel resource attributes matching the Vaka client's release, environment and service."""
    c = _api.get_client()
    attrs: Dict[str, Any] = {}
    if c is not None:
        o = c.options
        if service_name or o.service:
            attrs["service.name"] = service_name or o.service
        if o.release:
            attrs["service.version"] = o.release
        attrs["deployment.environment.name"] = o.environment
    elif service_name:
        attrs["service.name"] = service_name
    attrs["telemetry.distro.name"] = "vaka-python"
    if extra:
        attrs.update(extra)
    return attrs


class VakaSpanProcessor:
    """Adds ``session.id`` and ``enduser.id`` from the Vaka scope to every span that starts."""

    def on_start(self, span: Any, parent_context: Any = None) -> None:
        c = _api.get_client()
        if c is None:
            return
        if c.session:
            span.set_attribute("session.id", c.session["session_id"])
        user = current_scope().user_id
        if user:
            span.set_attribute("enduser.id", user)

    def _on_ending(self, span: Any) -> None:  # newer SDKs call this; nothing to do
        pass

    def on_end(self, span: Any) -> None:
        pass

    def shutdown(self) -> None:
        pass

    def force_flush(self, timeout_millis: int = 30000) -> bool:
        return True


def setup(
    service_name: Optional[str] = None,
    traces: bool = True,
    logs: bool = True,
    log_level: int = logging.INFO,
    resource: Optional[Mapping[str, Any]] = None,
    set_global: bool = True,
    endpoint: Optional[str] = None,
    key: Optional[str] = None,
) -> Tuple[Any, Any]:
    """Configure OpenTelemetry tracing and logging to export to Vaka.

    Call after ``vaka.init()``; endpoint and key come from the client unless given. Returns
    ``(tracer_provider, logger_provider)`` (``None`` for a disabled signal). With
    ``set_global=False`` the providers are only returned, for apps that manage their own.

    Spans get ``session.id`` and ``enduser.id`` from the Vaka scope, and errors captured by the
    SDK while a span is active carry its trace id. Log records at ``log_level`` and above are
    exported; ones the SDK already reported as errors are marked so Vaka doesn't count them twice.
    """
    _require()
    from opentelemetry.sdk.resources import Resource

    c = _api.get_client()
    endpoint = (endpoint or (c.options.endpoint if c else "")).rstrip("/")
    key = key or (c.options.key if c else "")
    if not endpoint or not key:
        raise ValueError("vaka.otel.setup: call vaka.init() first, or pass endpoint and key")
    headers = {"x-vaka-key": key}
    res = Resource.create(resource_attributes(service_name, resource))

    tracer_provider = None
    if traces:
        from opentelemetry import trace
        from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
        from opentelemetry.sdk.trace import TracerProvider
        from opentelemetry.sdk.trace.export import BatchSpanProcessor

        tracer_provider = TracerProvider(resource=res)
        tracer_provider.add_span_processor(VakaSpanProcessor())  # type: ignore[arg-type]
        tracer_provider.add_span_processor(
            BatchSpanProcessor(OTLPSpanExporter(endpoint=f"{endpoint}/ingest/otlp/v1/traces", headers=headers))
        )
        if set_global:
            trace.set_tracer_provider(tracer_provider)

    logger_provider = None
    if logs:
        from opentelemetry.exporter.otlp.proto.http._log_exporter import OTLPLogExporter
        from opentelemetry.sdk._logs import LoggerProvider, LoggingHandler
        from opentelemetry.sdk._logs.export import BatchLogRecordProcessor

        logger_provider = LoggerProvider(resource=res)
        logger_provider.add_log_record_processor(
            BatchLogRecordProcessor(OTLPLogExporter(endpoint=f"{endpoint}/ingest/otlp/v1/logs", headers=headers))
        )
        if set_global:
            try:
                from opentelemetry._logs import set_logger_provider

                set_logger_provider(logger_provider)
            except Exception:  # pragma: no cover
                pass
            import warnings

            with warnings.catch_warnings():
                # Newer SDKs point to opentelemetry-instrumentation-logging; the SDK handler
                # still works and avoids another dependency.
                warnings.simplefilter("ignore", DeprecationWarning)
                handler = LoggingHandler(level=log_level, logger_provider=logger_provider)
            handler.addFilter(_DropVakaInternal())
            logging.getLogger().addHandler(handler)

    if c is not None:
        c.otel_enabled = True  # type: ignore[attr-defined]
    return tracer_provider, logger_provider


class _DropVakaInternal(logging.Filter):
    def filter(self, record: logging.LogRecord) -> bool:
        return not record.name.startswith(("vaka", "opentelemetry"))
