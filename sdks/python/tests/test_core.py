import logging
import os
import subprocess
import sys
import textwrap
import threading

import vaka


class CartError(Exception):
    pass


def checkout(cart):
    if not cart:
        raise CartError("cart is empty")


def test_disabled_without_key(monkeypatch):
    monkeypatch.delenv("VAKA_KEY", raising=False)
    monkeypatch.delenv("VAKA_ENDPOINT", raising=False)
    assert vaka.init() is None
    assert vaka.capture_message("nothing") is None
    vaka.add_breadcrumb("x")
    vaka.track("x")


def test_capture_exception_payload(server, client):
    vaka.set_user("user-1")
    vaka.set_tag("region", "eu")
    vaka.add_breadcrumb("opened cart", category="navigation", data={"from": "/", "to": "/cart"})
    try:
        try:
            checkout([])
        except CartError as e:
            raise RuntimeError("checkout failed") from e
    except RuntimeError as e:
        event_id = vaka.capture_exception(e)
    assert event_id
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["event_id"] == event_id
    assert err["platform"] == "python"
    assert (err["release"], err["environment"]) == ("1.2.3", "test")
    assert err["handled"] is True and err["level"] == "error"
    assert err["user"] == {"id": "user-1"} and err["user_id"] == "user-1"
    assert err["tags"]["region"] == "eu"
    assert err["breadcrumbs"][0]["message"] == "opened cart"
    assert err["session_id"]
    outer, cause = err["exceptions"]
    assert outer["type"] == "RuntimeError" and outer["value"] == "checkout failed"
    assert cause["type"] == "test_core.CartError" and cause["value"] == "cart is empty"
    top = cause["stacktrace"][0]
    assert top["function"] == "checkout"
    assert top["in_app"] is True
    assert top["filename"].endswith("test_core.py")
    assert "raise CartError" in top["context_line"]
    assert top["pre_context"] and top["post_context"] is not None
    assert cause["stacktrace"][-1]["function"] == "test_capture_exception_payload"


def test_capture_exception_defaults_to_current(server, client):
    try:
        {}["missing"]
    except KeyError:
        vaka.capture_exception()
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["exceptions"][0]["type"] == "KeyError"


def test_same_exception_reported_once(server, client):
    e = ValueError("x")
    assert vaka.capture_exception(e)
    assert vaka.capture_exception(e) is None


def test_capture_message_has_caller_stack(server, client):
    vaka.capture_message("cache rebuilt", level="warning")
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["message"] == "cache rebuilt" and err["level"] == "warning"
    frames = err["exceptions"][0]["stacktrace"]
    assert frames[0]["function"] == "test_capture_message_has_caller_stack"


def test_library_frames_not_in_app(server, client):
    import json

    try:
        json.loads("{")
    except ValueError as e:
        vaka.capture_exception(e)
    [err] = server.wait_for(lambda it: it["type"] == "error")
    frames = err["exceptions"][0]["stacktrace"]
    assert frames[0]["in_app"] is False  # json/decoder.py
    assert any(f["in_app"] and f["function"] == "test_library_frames_not_in_app" for f in frames)


def test_in_app_include(server):
    vaka.init(key="vk", endpoint=server.url, in_app_include=["myapp"], flush_interval=0.2)
    try:
        raise ValueError("x")
    except ValueError as e:
        vaka.capture_exception(e)
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["exceptions"][0]["stacktrace"][0]["in_app"] is False


def test_sessions(server, client):
    vaka.capture_message("bad", level="error")
    vaka.shutdown()
    sessions = [it for it in server.items("session")]
    assert sessions[0]["status"] == "ok"
    assert sessions[-1]["status"] == "errored"
    assert sessions[-1]["errors"] == 1
    assert sessions[0]["session_id"] == sessions[-1]["session_id"]
    assert sessions[-1]["release"] == "1.2.3"


def test_clean_exit_session(server, client):
    vaka.shutdown()
    assert server.items("session")[-1]["status"] == "exited"


def test_track(server, client):
    vaka.track("job_finished", {"kind": "import", "rows": 3, "obj": object()})
    [ev] = server.wait_for(lambda it: it["type"] == "event")
    assert ev["name"] == "job_finished"
    assert ev["props"]["kind"] == "import" and ev["props"]["rows"] == 3
    assert isinstance(ev["props"]["obj"], str)
    assert ev["anon_id"] and ev["session_id"]


def test_isolation_scope(server, client):
    vaka.set_tag("global", "1")
    with vaka.isolation_scope():
        vaka.set_tag("job", "import")
        vaka.add_breadcrumb("inside")
        vaka.capture_message("in job")
    vaka.capture_message("outside")
    errs = server.wait_for(lambda it: it["type"] == "error" and it["message"] == "outside")
    inside = [e for e in server.items("error") if e["message"] == "in job"][0]
    assert inside["tags"]["job"] == "import" and inside["tags"]["global"] == "1"
    assert "job" not in errs[0]["tags"]
    assert not errs[0]["breadcrumbs"]


def test_breadcrumbs_capped(server, client):
    for i in range(150):
        vaka.add_breadcrumb(f"b{i}")
    vaka.capture_message("x")
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert len(err["breadcrumbs"]) == 100
    assert err["breadcrumbs"][-1]["message"] == "b149"


def test_before_send(server):
    def drop_secret(item):
        if item.get("message") == "secret":
            return None
        item["tags"]["scrubbed"] = "yes"
        return item

    vaka.init(key="vk", endpoint=server.url, before_send=drop_secret, flush_interval=0.2)
    vaka.capture_message("secret")
    vaka.capture_message("public")
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["message"] == "public" and err["tags"]["scrubbed"] == "yes"


def test_logging_integration(server, client):
    log = logging.getLogger("shop.cart")
    log.setLevel(logging.INFO)
    log.info("loaded cart %s", 42)
    log.error("payment declined", extra={"order": 7})
    try:
        1 / 0
    except ZeroDivisionError:
        log.exception("division failed")
    errs = server.wait_for(lambda it: it["type"] == "error" and it.get("exceptions", [{}])[0].get("type") == "ZeroDivisionError")
    msg = [e for e in server.items("error") if e.get("message") == "payment declined"][0]
    assert msg["tags"]["logger"] == "shop.cart"
    assert msg["contexts"]["log"]["extra"]["order"] == 7
    assert msg["breadcrumbs"][-1]["message"] == "loaded cart 42"
    assert msg["breadcrumbs"][-1]["category"] == "console"
    assert errs[0]["tags"]["mechanism"] == "logging"
    assert errs[0]["breadcrumbs"][-1]["message"] == "payment declined"


def test_logging_lastresort_untouched(server, client, capsys):
    # The SDK must not add a root handler: the app's output stays the same.
    assert not any(type(h).__name__ == "VakaHandler" for h in logging.getLogger().handlers)


def test_threading_excepthook(server, client):
    def boom():
        raise ValueError("thread failed")

    t = threading.Thread(target=boom)
    t.start()
    t.join()
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["level"] == "fatal" and err["handled"] is False
    assert err["tags"]["mechanism"] == "threading"


def test_unhandled_exception_in_subprocess(server, tmp_path):
    script = tmp_path / "app.py"
    script.write_text(
        textwrap.dedent(
            f"""
            import vaka
            vaka.init(key="vk", endpoint="{server.url}", release="9.9.9")
            def main():
                raise LookupError("no such order")
            main()
            """
        )
    )
    src = os.path.join(os.path.dirname(__file__), "..", "src")
    env = dict(os.environ, PYTHONPATH=src)
    proc = subprocess.run([sys.executable, str(script)], env=env, capture_output=True, text=True, timeout=30)
    assert proc.returncode == 1
    assert "LookupError: no such order" in proc.stderr  # the default hook still prints
    errs = server.items("error")
    assert len(errs) == 1
    err = errs[0]
    assert err["fatal"] is True and err["handled"] is False and err["tags"]["mechanism"] == "excepthook"
    assert err["exceptions"][0]["stacktrace"][0]["function"] == "main"
    statuses = [s["status"] for s in server.items("session")]
    assert statuses[-1] == "crashed"


def test_monitor_decorator(server, client):
    @vaka.monitor
    def job():
        raise KeyError("x")

    try:
        job()
    except KeyError:
        pass
    [err] = server.wait_for(lambda it: it["type"] == "error")
    assert err["tags"]["function"].endswith("job") and err["handled"] is False
