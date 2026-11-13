import pytest

import vaka

TRACEPARENT = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"


def _request_error(server, mechanism):
    [err] = server.wait_for(lambda it: it["type"] == "error" and it["tags"].get("mechanism") == mechanism)
    return err


def test_flask(server, client):
    flask = pytest.importorskip("flask")
    from vaka.integrations.flask import init_app

    app = flask.Flask(__name__)
    init_app(app)

    @app.route("/carts/<int:cart_id>")
    def cart(cart_id):
        vaka.add_breadcrumb("loading cart", category="db")
        vaka.set_tag("cart", str(cart_id))
        raise ValueError(f"cart {cart_id} missing")

    @app.route("/ok")
    def ok():
        vaka.set_tag("leak", "yes")
        return "ok"

    c = app.test_client()
    assert c.get("/ok").status_code == 200
    assert c.get("/carts/5?x=1", headers={"traceparent": TRACEPARENT}).status_code == 500
    err = _request_error(server, "flask")
    assert err["handled"] is False
    assert err["url"] == "http://localhost/carts/5?x=1"
    assert err["tags"]["transaction"] == "GET /carts/<int:cart_id>"
    assert err["tags"]["cart"] == "5" and "leak" not in err["tags"]
    assert err["trace_id"] == "4bf92f3577b34da6a3ce929d0e0e4736"
    assert [b["message"] for b in err["breadcrumbs"]] == ["loading cart"]
    assert err["contexts"]["request"] == {"method": "GET", "url": "http://localhost/carts/5?x=1"}
    # Flask also logs the exception via app.logger; it must not become a second error.
    vaka.flush()
    assert len(server.items("error")) == 1


def test_fastapi(server, client):
    fastapi = pytest.importorskip("fastapi")
    from fastapi.testclient import TestClient

    from vaka.integrations.asgi import VakaAsgiMiddleware

    app = fastapi.FastAPI()
    app.add_middleware(VakaAsgiMiddleware)

    @app.get("/orders/{order_id}")
    async def order(order_id: int):
        vaka.set_tag("order", order_id)
        raise KeyError(order_id)

    @app.get("/missing")
    async def missing():
        raise fastapi.HTTPException(status_code=404)

    tc = TestClient(app, raise_server_exceptions=False)
    assert tc.get("/missing").status_code == 404
    assert tc.get("/orders/9", headers={"traceparent": TRACEPARENT}).status_code == 500
    err = _request_error(server, "asgi")
    assert err["exceptions"][0]["type"] == "KeyError"
    assert err["tags"]["transaction"] == "GET /orders/{order_id}"
    assert err["tags"]["order"] == "9"
    assert err["url"] == "http://testserver/orders/9"
    assert err["trace_id"] == "4bf92f3577b34da6a3ce929d0e0e4736"
    assert len(server.items("error")) == 1  # HTTPException is not an error


def test_django(server, client):
    pytest.importorskip("django")
    from django.conf import settings

    if not settings.configured:
        settings.configure(
            DEBUG=False,
            ALLOWED_HOSTS=["*"],
            ROOT_URLCONF=__name__,
            MIDDLEWARE=["vaka.integrations.django.VakaMiddleware"],
            SECRET_KEY="test",
        )
    import django

    django.setup()
    from django.test import Client

    c = Client(raise_request_exception=False)
    assert c.get("/items/3/", HTTP_TRACEPARENT=TRACEPARENT).status_code == 500
    err = _request_error(server, "django")
    assert err["exceptions"][0]["type"] == "ZeroDivisionError"
    assert err["tags"]["transaction"] == "GET /items/<int:pk>/"
    assert err["url"] == "http://testserver/items/3/"
    assert err["trace_id"] == "4bf92f3577b34da6a3ce929d0e0e4736"
    vaka.flush()
    assert len(server.items("error")) == 1  # django.request's error log is deduplicated


def _item_view(request, pk):
    return 1 / 0


try:
    from django.urls import path

    urlpatterns = [path("items/<int:pk>/", _item_view)]
except ImportError:  # pragma: no cover
    urlpatterns = []


def test_wsgi_streaming_error(server, client):
    from vaka.integrations.wsgi import VakaWsgiMiddleware

    def app(environ, start_response):
        start_response("200 OK", [("Content-Type", "text/plain")])

        def body():
            yield b"a"
            raise IOError("stream broke")

        return body()

    wrapped = VakaWsgiMiddleware(app)
    environ = {"REQUEST_METHOD": "GET", "PATH_INFO": "/stream", "HTTP_HOST": "example.com", "wsgi.url_scheme": "https"}
    with pytest.raises(OSError):
        list(wrapped(environ, lambda *a: None))
    err = _request_error(server, "wsgi")
    assert err["url"] == "https://example.com/stream"
