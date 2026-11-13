import logging

import pytest

import vaka

pytest.importorskip("opentelemetry.sdk")
pytest.importorskip("opentelemetry.exporter.otlp.proto.http")


def _attrs(kvs):
    out = {}
    for kv in kvs:
        v = kv.value
        out[kv.key] = getattr(v, v.WhichOneof("value")) if v.WhichOneof("value") else None
    return out


def test_otel_exports_spans_and_logs(server):
    from opentelemetry import trace
    from opentelemetry.proto.collector.logs.v1.logs_service_pb2 import ExportLogsServiceRequest
    from opentelemetry.proto.collector.trace.v1.trace_service_pb2 import ExportTraceServiceRequest

    import vaka.otel

    vaka.init(key="vk_otel", endpoint=server.url, release="3.0.0", environment="staging", service="billing", flush_interval=0.2)
    vaka.set_user("u-1")
    tp, lp = vaka.otel.setup(set_global=False)
    tracer = tp.get_tracer("test")
    log = logging.getLogger("billing.jobs")
    log.setLevel(logging.INFO)
    from opentelemetry.sdk._logs import LoggingHandler

    handler = LoggingHandler(logger_provider=lp)
    log.addHandler(handler)
    try:
        with tracer.start_as_current_span("charge", kind=trace.SpanKind.SERVER) as span:
            trace_id = format(span.get_span_context().trace_id, "032x")
            log.info("charging")
            try:
                raise RuntimeError("card declined")
            except RuntimeError as e:
                log.exception("charge failed")
                with pytest.raises(RuntimeError):
                    raise e
    except RuntimeError:
        pass
    finally:
        log.removeHandler(handler)
    tp.force_flush()
    lp.force_flush()

    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["trace_id"] == trace_id
    assert err["tags"]["service"] == "billing"

    trace_reqs = [(h, b) for p, h, b in server.requests if p == "/ingest/otlp/v1/traces"]
    assert trace_reqs and trace_reqs[0][0]["x-vaka-key"] == "vk_otel"
    req = ExportTraceServiceRequest.FromString(trace_reqs[0][1])
    rs = req.resource_spans[0]
    res = _attrs(rs.resource.attributes)
    assert res["service.name"] == "billing"
    assert res["service.version"] == "3.0.0"
    assert res["deployment.environment.name"] == "staging"
    sp = rs.scope_spans[0].spans[0]
    a = _attrs(sp.attributes)
    assert a["enduser.id"] == "u-1"
    assert a["session.id"] == vaka.get_client().session["session_id"]
    assert a["vaka.error_captured"] is True  # the exception event won't become a duplicate error

    log_reqs = [b for p, _, b in server.requests if p == "/ingest/otlp/v1/logs"]
    records = [r for b in log_reqs for rl in ExportLogsServiceRequest.FromString(b).resource_logs for sl in rl.scope_logs for r in sl.log_records]
    by_body = {r.body.string_value: r for r in records}
    assert format(int.from_bytes(by_body["charging"].trace_id, "big"), "032x") == trace_id
    assert "vaka.error_captured" not in _attrs(by_body["charging"].attributes)
    assert _attrs(by_body["charge failed"].attributes)["vaka.error_captured"] is True
    tp.shutdown()
    lp.shutdown()


def test_setup_requires_init():
    import vaka.otel

    with pytest.raises(ValueError):
        vaka.otel.setup(set_global=False)
