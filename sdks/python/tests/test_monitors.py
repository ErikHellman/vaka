import asyncio
import json

import pytest

import vaka


def check_ins(server, slug):
    with server.lock:
        return [(h, json.loads(b)) for p, h, b in server.requests if p == f"/ingest/v1/monitors/{slug}/check-ins"]


def test_check_in(server, client):
    assert vaka.check_in("heartbeat") is True
    [(headers, body)] = check_ins(server, "heartbeat")
    assert headers["X-Vaka-Key"] == "vk_test"
    assert body["status"] == "ok"
    assert body["release"] == "1.2.3"
    assert vaka.check_in("missing", "error", message="boom") is False
    with pytest.raises(ValueError):
        vaka.check_in("heartbeat", "done")


def test_monitor_decorator_reports_run(server, client):
    @vaka.monitor("nightly-backup", schedule="0 3 * * *", timezone="Europe/Stockholm", grace_s=600, max_runtime_s=3600)
    def backup():
        return 42

    assert backup() == 42
    [(_, start), (_, done)] = check_ins(server, "nightly-backup")
    assert start["status"] == "in_progress"
    assert start["monitor"] == {"schedule": {"cron": "0 3 * * *", "timezone": "Europe/Stockholm"}, "grace_s": 600, "max_runtime_s": 3600}
    assert done["status"] == "ok"
    assert done["check_in_id"] == start["check_in_id"]
    assert done["duration_ms"] >= 0
    assert "monitor" not in done


def test_monitor_context_manager_error(server, client):
    with pytest.raises(RuntimeError):
        with vaka.monitor("sync", every="10m"):
            raise RuntimeError("sync broke")
    [(_, start), (_, failed)] = check_ins(server, "sync")
    assert start["monitor"] == {"schedule": {"every": "10m"}}
    assert failed["status"] == "error"
    assert failed["message"] == "sync broke"
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["tags"]["monitor.check_in_id"] == start["check_in_id"]
    assert err["tags"]["monitor.slug"] == "sync"
    # The run's tags don't leak out of the run.
    assert "monitor.check_in_id" not in vaka.current_scope().tags


def test_monitor_async(server, client):
    @vaka.monitor("async-job")
    async def job():
        return "done"

    assert asyncio.run(job()) == "done"
    assert [b["status"] for _, b in check_ins(server, "async-job")] == ["in_progress", "ok"]


def test_plain_monitor_decorator_still_works(server, client):
    @vaka.monitor
    def f():
        return 1

    assert f() == 1
