import json

import pytest

import vaka

PNG = b"\x89PNG\r\n\x1a\n" + b"\x00" * 16


def feedback_requests(server):
    with server.lock:
        return [(h, b) for p, h, b in server.requests if p.startswith("/ingest/v1/feedback")]


def test_capture_feedback_json(server, client):
    vaka.set_user("user-7")
    vaka.set_tag("tenant", "acme")
    event_id = vaka.capture_exception(ValueError("export failed"))
    r = vaka.capture_feedback(
        "  The export button does nothing  ",
        email="ann@example.com",
        event_id=event_id,
        issue="API-12",
        session_id="s-1",
        ip="203.0.113.9",
        tags={"plan": "pro"},
    )
    assert r == vaka.FeedbackResult("fb-1", "API-F1")
    [(headers, body)] = feedback_requests(server)
    assert headers["X-Vaka-Key"] == "vk_test"
    assert headers["X-Forwarded-For"] == "203.0.113.9"
    item = json.loads(body)
    assert item["message"] == "The export button does nothing"
    assert item["email"] == "ann@example.com"
    assert item["event_id"] == event_id
    assert item["issue"] == "API-12"
    assert item["session_id"] == "s-1"
    assert item["user_id"] == "user-7"
    assert item["source"] == "api"
    assert item["platform"] == "python"
    assert item["release"] == "1.2.3"
    assert item["tags"] == {"tenant": "acme", "plan": "pro"}
    assert len(item["id"]) == 36


def test_capture_feedback_multipart(server, client):
    vaka.capture_feedback("with a screenshot", attachments=[("shot.png", PNG)])
    [(headers, body)] = feedback_requests(server)
    assert headers["Content-Type"].startswith("multipart/form-data; boundary=")
    raw = body.decode("latin1")
    assert 'name="feedback"' in raw and '"message": "with a screenshot"' in raw
    assert 'filename="shot.png"\r\nContent-Type: image/png' in raw
    assert PNG.decode("latin1") in raw


def test_capture_feedback_errors(server, client):
    with pytest.raises(vaka.FeedbackError):
        vaka.capture_feedback("   ")
    with pytest.raises(vaka.FeedbackError, match="at most 3"):
        vaka.capture_feedback("x", attachments=[("a.png", PNG)] * 4)
    server.feedback_status = 429
    with pytest.raises(vaka.FeedbackError) as e:
        vaka.capture_feedback("again")
    assert e.value.status == 429
    assert "too much feedback" in str(e.value)


def test_capture_feedback_not_initialized():
    with pytest.raises(vaka.FeedbackError, match="not initialized"):
        vaka.capture_feedback("hello")
