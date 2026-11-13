import json
import time

import pytest

import vaka
from vaka._transport import Transport


def test_batches_and_sdk_header(server):
    t = Transport(server.url, "vk_k", flush_interval=0.1)
    for i in range(3):
        t.send({"type": "event", "name": f"e{i}"})
    assert t.flush(2)
    [batch] = server.batches()
    assert batch["sdk"]["name"] == "vaka-python"
    assert [it["name"] for it in batch["items"]] == ["e0", "e1", "e2"]
    path, headers, _ = server.requests[0]
    assert headers["X-Vaka-Key"] == "vk_k"
    t.close()


def test_large_batches_are_gzipped(server):
    t = Transport(server.url, "vk", flush_interval=0.1)
    for i in range(40):
        t.send({"type": "event", "name": "x" * 200})
    t.flush(2)
    assert server.requests[0][1].get("Content-Encoding") == "gzip"
    assert len(server.items()) == 40
    t.close()


def test_retries_after_503(server):
    server.statuses = [503]
    t = Transport(server.url, "vk", flush_interval=0.1)
    t.send({"type": "error", "event_id": "1"})
    t.flush(2)
    assert len(server.batches()) == 1  # rejected
    assert t._backoff == 5.0
    t._backoff = 0  # skip the wait in the test
    t.flush(2)
    assert [it["event_id"] for it in server.items()] == ["1", "1"]
    t.close()


def test_401_disables(server):
    server.statuses = [401]
    t = Transport(server.url, "vk_bad", flush_interval=0.1)
    t.send({"type": "event", "name": "a"})
    t.flush(2)
    t.send({"type": "event", "name": "b"})
    t.flush(2)
    assert len(server.requests) == 1
    t.close()


def test_413_splits(server):
    server.statuses = [413]
    t = Transport(server.url, "vk", flush_interval=10)
    for i in range(4):
        t.send({"type": "event", "name": str(i)})
    t.flush(2)
    sizes = [len(b["items"]) for b in server.batches()]
    assert sizes == [4, 2, 2]
    t.close()


def test_network_error_requeues():
    t = Transport("http://127.0.0.1:9", "vk", flush_interval=0.1)
    t.send({"type": "event", "name": "a"})
    t.flush(3)
    assert len(t._queue) == 1 and t._backoff >= 5
    t.close(0.1)


def test_load_flags_and_exposure(server, client):
    flags = vaka.load_flags({"plan": "pro"})
    body = json.loads([b for p, _, b in server.requests if p == "/ingest/v1/flags"][0])
    assert body["distinct_id"] == client.distinct_id
    assert body["environment"] == "test"
    assert body["properties"] == {"plan": "pro"}
    assert flags.peek("new_checkout") == "one_page"
    assert vaka.get_flag("new_checkout") == "one_page"
    assert vaka.is_enabled("dark_mode") is True
    assert vaka.is_enabled("beta") is False
    vaka.track("purchase")
    [ev] = server.wait_for(lambda it: it["type"] == "event")
    assert ev["flags"] == {"new_checkout": "one_page", "dark_mode": "true", "beta": "false"}
    vaka.shutdown()
    assert server.items("session")[-1]["flags"]["new_checkout"] == "one_page"


def test_evaluate_flags_per_request(server, client):
    with vaka.isolation_scope():
        f = vaka.evaluate_flags("user-7", user_id="user-7", properties={"plan": "enterprise"})
        assert f.get("new_checkout") == "one_page"
        vaka.capture_message("in request")
    vaka.capture_message("outside")
    server.wait_for(lambda it: it.get("message") == "outside")
    by_msg = {e["message"]: e for e in server.items("error")}
    assert by_msg["in request"]["flags"] == {"new_checkout": "one_page"}
    assert "flags" not in by_msg["outside"]
    body = json.loads([b for p, _, b in server.requests if p == "/ingest/v1/flags"][0])
    assert body["user_id"] == "user-7"


def test_flags_option_loads_in_background(server):
    vaka.init(key="vk", endpoint=server.url, flags=True, flag_properties={"tier": "gold"}, flush_interval=0.2)
    deadline = time.time() + 3
    while vaka.get_client().flags is None and time.time() < deadline:
        time.sleep(0.02)
    assert vaka.get_flag("new_checkout") == "one_page"


def test_flag_errors_raise(server, client):
    server.flags = None
    vaka.get_client().options.endpoint = "http://127.0.0.1:9"
    with pytest.raises(Exception):
        vaka.load_flags(timeout=0.5)
