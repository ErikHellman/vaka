import gzip
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

import vaka


class FakeVaka:
    """A tiny stand-in for the Vaka ingest API that records what it receives."""

    def __init__(self):
        self.requests = []  # (path, headers, body bytes)
        self.statuses = []  # queued status codes for /ingest/v1/batch
        self.flags = {"new_checkout": "one_page", "beta": False, "dark_mode": True}
        self.lock = threading.Lock()
        fake = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                n = int(self.headers.get("Content-Length", 0))
                body = self.rfile.read(n)
                if self.headers.get("Content-Encoding") == "gzip":
                    body = gzip.decompress(body)
                with fake.lock:
                    fake.requests.append((self.path, dict(self.headers), body))
                    status = fake.statuses.pop(0) if self.path.startswith("/ingest/v1/batch") and fake.statuses else None
                if self.path.startswith("/ingest/v1/flags"):
                    self._reply(200, {"flags": fake.flags, "details": {}})
                elif self.path.startswith("/ingest/otlp"):
                    self._reply(200, {}, content_type=self.headers.get("Content-Type", "application/json"), raw=b"")
                elif status:
                    self._reply(status, {"error": f"status {status}"})
                else:
                    items = json.loads(body)["items"]
                    self._reply(202, {"accepted": len(items), "rejected": []})

            def _reply(self, status, obj, content_type="application/json", raw=None):
                data = raw if raw is not None else json.dumps(obj).encode()
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def batches(self):
        with self.lock:
            return [json.loads(b) for p, _, b in self.requests if p.startswith("/ingest/v1/batch")]

    def items(self, type=None):
        out = [it for b in self.batches() for it in b["items"]]
        return [it for it in out if type is None or it["type"] == type]

    def wait_for(self, pred, timeout=5.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            vaka.flush(1.0)
            found = [it for it in self.items() if pred(it)]
            if found:
                return found
            time.sleep(0.05)
        raise AssertionError(f"timed out; got {self.items()}")

    def close(self):
        self.server.shutdown()
        self.server.server_close()


@pytest.fixture
def server():
    s = FakeVaka()
    yield s
    vaka.shutdown(timeout=1)
    s.close()


@pytest.fixture
def client(server):
    c = vaka.init(key="vk_test", endpoint=server.url, release="1.2.3", environment="test", flush_interval=0.2)
    yield c
    vaka.shutdown(timeout=2)


@pytest.fixture(autouse=True)
def _reset():
    yield
    vaka.shutdown(timeout=1)
    vaka._client._global_scope.__init__()
