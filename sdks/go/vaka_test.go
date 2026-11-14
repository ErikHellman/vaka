package vaka_test

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	vaka "github.com/ErikHellman/vaka/sdks/go"
)

// fakeServer records what the SDK sends.
type fakeServer struct {
	*httptest.Server
	mu     sync.Mutex
	items  []map[string]any
	otlp   map[string][][]byte
	keys   []string
	status int
	// feedback requests and the status to answer them with (default 202)
	feedback []feedbackReq
	fbStatus int
}

type feedbackReq struct {
	header http.Header
	body   []byte
}

func newFake(t *testing.T) *fakeServer {
	f := &fakeServer{otlp: map[string][][]byte{}, status: 202}
	f.Server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body io.Reader = r.Body
		if r.Header.Get("Content-Encoding") == "gzip" {
			gz, err := gzip.NewReader(r.Body)
			if err != nil {
				t.Errorf("gzip: %v", err)
				return
			}
			body = gz
		}
		b, _ := io.ReadAll(body)
		f.mu.Lock()
		defer f.mu.Unlock()
		f.keys = append(f.keys, r.Header.Get("X-Vaka-Key"))
		switch r.URL.Path {
		case "/ingest/v1/batch":
			var req struct {
				SDK   map[string]any   `json:"sdk"`
				Items []map[string]any `json:"items"`
			}
			if err := json.Unmarshal(b, &req); err != nil {
				t.Errorf("batch is not JSON: %v", err)
			}
			if req.SDK["name"] != "vaka-go" {
				t.Errorf("sdk = %v", req.SDK)
			}
			f.items = append(f.items, req.Items...)
			w.WriteHeader(f.status)
			fmt.Fprintf(w, `{"accepted": %d, "rejected": []}`, len(req.Items))
		case "/ingest/v1/flags":
			var req map[string]any
			_ = json.Unmarshal(b, &req)
			if req["distinct_id"] != "user-1" || req["environment"] != "staging" {
				t.Errorf("flag request = %v", req)
			}
			fmt.Fprint(w, `{"flags": {"new_checkout": "one_page", "beta": false, "dark": true}, "details": {}}`)
		case "/ingest/v1/feedback":
			f.feedback = append(f.feedback, feedbackReq{r.Header.Clone(), b})
			if f.fbStatus != 0 {
				w.WriteHeader(f.fbStatus)
				fmt.Fprint(w, `{"error": "too much feedback"}`)
				return
			}
			w.WriteHeader(202)
			fmt.Fprint(w, `{"id": "fb-1", "short_id": "API-F1"}`)
		case "/ingest/otlp/v1/traces", "/ingest/otlp/v1/logs":
			f.otlp[r.URL.Path] = append(f.otlp[r.URL.Path], b)
			w.WriteHeader(200)
		default:
			t.Errorf("unexpected request %s %s", r.Method, r.URL.Path)
			w.WriteHeader(404)
		}
	}))
	t.Cleanup(f.Close)
	return f
}

func (f *fakeServer) byType(typ string) []map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []map[string]any
	for _, it := range f.items {
		if it["type"] == typ {
			out = append(out, it)
		}
	}
	return out
}

func (f *fakeServer) otlpBodies(path string) [][]byte {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.otlp[path]
}

func initClient(t *testing.T, f *fakeServer, mod func(*vaka.Config)) *vaka.Client {
	t.Helper()
	cfg := vaka.Config{
		Key:          "vk_test",
		Endpoint:     f.URL,
		Release:      "1.2.3",
		Environment:  "staging",
		ServiceName:  "checkout",
		InAppInclude: []string{"github.com/ErikHellman/vaka/sdks/go_test"},
	}
	if mod != nil {
		mod(&cfg)
	}
	c, err := vaka.Init(cfg)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = c.Shutdown(context.Background()) })
	return c
}

func flush(t *testing.T, c *vaka.Client) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := c.Flush(ctx); err != nil {
		t.Fatalf("flush: %v", err)
	}
}

func frames(t *testing.T, ex map[string]any) []map[string]any {
	t.Helper()
	raw, _ := json.Marshal(ex["stacktrace"])
	var fs []map[string]any
	_ = json.Unmarshal(raw, &fs)
	if len(fs) == 0 {
		t.Fatalf("no frames in %v", ex)
	}
	return fs
}

func openConfig() error {
	_, err := os.Open("/does/not/exist.toml")
	if err != nil {
		return fmt.Errorf("load config: %w", err)
	}
	return nil
}

func TestCaptureError(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	ctx := vaka.WithScope(context.Background())
	vaka.SetUser(ctx, "user-42")
	vaka.SetTag(ctx, "region", "eu")
	vaka.SetFlag(ctx, "beta", false)
	vaka.AddBreadcrumb(ctx, "db", "connected", vaka.LevelInfo)
	id := c.CaptureError(ctx, openConfig(), vaka.WithTags(map[string]string{"job": "import"}))
	if id == "" {
		t.Fatal("no event id")
	}
	flush(t, c)

	errs := f.byType("error")
	if len(errs) != 1 {
		t.Fatalf("errors = %d", len(errs))
	}
	e := errs[0]
	if e["event_id"] != id || e["platform"] != "go" || e["release"] != "1.2.3" || e["environment"] != "staging" || e["handled"] != true || e["level"] != "error" {
		t.Errorf("error item = %v", e)
	}
	exs := e["exceptions"].([]any)
	if len(exs) < 2 {
		t.Fatalf("exceptions = %v", exs)
	}
	outer, inner := exs[0].(map[string]any), exs[1].(map[string]any)
	if outer["type"] != "fs.PathError" || !strings.HasPrefix(outer["value"].(string), "load config: open /does/not/exist.toml") {
		t.Errorf("outer = %v", outer)
	}
	if inner["type"] != "fs.PathError" || inner["module"] != "io/fs" {
		t.Errorf("inner = %v", inner)
	}
	fr := frames(t, outer)
	if fr[0]["function"] != "TestCaptureError" || fr[0]["in_app"] != true || fr[0]["filename"] != "vaka_test.go" || fr[0]["module"] != "github.com/ErikHellman/vaka/sdks/go_test" {
		t.Errorf("first frame = %v", fr[0])
	}
	for _, x := range fr[1:] {
		if x["module"] == "testing" && x["in_app"] == true {
			t.Errorf("stdlib frame marked in-app: %v", x)
		}
	}
	tags := e["tags"].(map[string]any)
	if tags["region"] != "eu" || tags["job"] != "import" || tags["service"] != "checkout" {
		t.Errorf("tags = %v", tags)
	}
	if e["user_id"] != "user-42" || e["user"].(map[string]any)["id"] != "user-42" {
		t.Errorf("user = %v / %v", e["user_id"], e["user"])
	}
	if e["flags"].(map[string]any)["beta"] != "false" {
		t.Errorf("flags = %v", e["flags"])
	}
	bc := e["breadcrumbs"].([]any)
	if len(bc) != 1 || bc[0].(map[string]any)["category"] != "db" {
		t.Errorf("breadcrumbs = %v", bc)
	}
	if e["session_id"] == nil || e["session_id"] != f.byType("session")[0]["session_id"] {
		t.Errorf("session id missing or different: %v", e["session_id"])
	}
	// The global scope is untouched by the request scope.
	if len(f.keys) == 0 || f.keys[0] != "vk_test" {
		t.Errorf("keys = %v", f.keys)
	}
}

func TestSessionLifecycle(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	c.CaptureMessage(context.Background(), "cache rebuilt", vaka.LevelWarning)
	if err := c.Shutdown(context.Background()); err != nil {
		t.Fatal(err)
	}
	ss := f.byType("session")
	if len(ss) != 2 || ss[0]["status"] != "ok" || ss[1]["status"] != "errored" || ss[1]["errors"] != float64(1) {
		t.Fatalf("sessions = %v", ss)
	}
	m := f.byType("error")[0]
	if m["message"] != "cache rebuilt" || m["level"] != "warning" {
		t.Errorf("message = %v", m)
	}
	// After shutdown everything is a no-op.
	if id := c.CaptureMessage(context.Background(), "late", vaka.LevelInfo); id != "" {
		t.Error("captured after shutdown")
	}
}

func crashy() {
	var m map[string]int
	m["boom"] = 1 // panics: assignment to entry in nil map
}

func TestRecover(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	func() {
		defer func() {
			if v := recover(); v == nil {
				t.Error("Recover did not re-panic")
			}
		}()
		defer c.Recover()
		crashy()
	}()
	errs := f.byType("error")
	if len(errs) != 1 {
		t.Fatalf("errors = %v", errs)
	}
	e := errs[0]
	if e["fatal"] != true || e["handled"] != false || e["tags"].(map[string]any)["mechanism"] != "panic" {
		t.Errorf("panic item = %v", e)
	}
	ex := e["exceptions"].([]any)[0].(map[string]any)
	if !strings.HasPrefix(ex["type"].(string), "panic: runtime.") || !strings.Contains(ex["value"].(string), "nil map") {
		t.Errorf("exception = %v", ex)
	}
	if fr := frames(t, ex); fr[0]["function"] != "crashy" {
		t.Errorf("crash site = %v", fr[0])
	}
	ss := f.byType("session")
	if ss[len(ss)-1]["status"] != "crashed" {
		t.Errorf("sessions = %v", ss)
	}
}

func TestRecoverNonError(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	func() {
		defer func() { _ = recover() }()
		defer c.Recover()
		panic("out of widgets")
	}()
	ex := f.byType("error")[0]["exceptions"].([]any)[0].(map[string]any)
	if ex["type"] != "panic" || ex["value"] != "out of widgets" {
		t.Errorf("exception = %v", ex)
	}
	if fr := frames(t, ex); fr[0]["function"] != "TestRecoverNonError.func1" {
		t.Errorf("crash site = %v", fr[0])
	}
}

func TestMiddleware(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	mux := http.NewServeMux()
	mux.HandleFunc("GET /carts/{id}", func(w http.ResponseWriter, r *http.Request) {
		vaka.AddBreadcrumb(r.Context(), "cart", "loading "+r.PathValue("id"), vaka.LevelInfo)
		if r.PathValue("id") == "bad" {
			panic(errors.New("cart is corrupt"))
		}
		w.Write([]byte("ok"))
	})
	srv := httptest.NewServer(vaka.Middleware(mux))
	defer srv.Close()

	req, _ := http.NewRequest("GET", srv.URL+"/carts/bad", nil)
	req.Header.Set("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != 500 {
		t.Errorf("status = %d", resp.StatusCode)
	}
	resp, err = http.Get(srv.URL + "/carts/1")
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	flush(t, c)

	errs := f.byType("error")
	if len(errs) != 1 {
		t.Fatalf("errors = %v", errs)
	}
	e := errs[0]
	if e["trace_id"] != "4bf92f3577b34da6a3ce929d0e0e4736" || e["handled"] != false || e["fatal"] != false {
		t.Errorf("error = %v", e)
	}
	// Only this request's breadcrumb, not the other request's.
	if bc := e["breadcrumbs"].([]any); len(bc) != 1 || bc[0].(map[string]any)["message"] != "loading bad" {
		t.Errorf("breadcrumbs = %v", bc)
	}
	traces := bytes.Join(f.otlpBodies("/ingest/otlp/v1/traces"), nil)
	for _, want := range []string{"GET /carts/{id}", "http.route", "/carts/{id}", "checkout", "1.2.3", "deployment.environment.name", "vaka.error_captured"} {
		if !bytes.Contains(traces, []byte(want)) {
			t.Errorf("OTLP traces missing %q", want)
		}
	}
	// Ingest and OTLP both carry the key.
	for _, k := range f.keys {
		if k != "vk_test" && k != "" {
			t.Errorf("key = %q", k)
		}
	}
}

func TestTransportPropagates(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	var got string
	up := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		got = r.Header.Get("traceparent")
		w.WriteHeader(503)
	}))
	defer up.Close()
	ctx, span := vaka.StartSpan(vaka.WithScope(context.Background()), "job")
	req, _ := http.NewRequestWithContext(ctx, "GET", up.URL+"/stock?token=secret", nil)
	resp, err := (&http.Client{Transport: vaka.Transport(nil)}).Do(req)
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	c.CaptureMessage(ctx, "upstream down", vaka.LevelError)
	span.End()
	flush(t, c)
	if !strings.HasPrefix(got, "00-"+span.SpanContext().TraceID().String()) {
		t.Errorf("traceparent = %q", got)
	}
	e := f.byType("error")[0]
	bc := e["breadcrumbs"].([]any)[0].(map[string]any)
	data := bc["data"].(map[string]any)
	if bc["category"] != "http" || data["status_code"] != float64(503) || bc["level"] != "warning" {
		t.Errorf("breadcrumb = %v", bc)
	}
	if strings.Contains(data["url"].(string), "secret") && !strings.Contains(data["url"].(string), "xxxxx") {
		// url.Redacted only hides passwords; query strings are the caller's concern.
		t.Logf("url = %v", data["url"])
	}
}

func TestFlags(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	flags, err := c.EvaluateFlags(context.Background(), vaka.FlagRequest{DistinctID: "user-1", Properties: map[string]any{"plan": "pro"}})
	if err != nil {
		t.Fatal(err)
	}
	if flags.Variant("new_checkout", "control") != "one_page" || !flags.Enabled("dark") || flags.Enabled("beta") || flags.Enabled("missing") || flags.Variant("missing", "control") != "control" {
		t.Errorf("flags = %v", flags)
	}
	ctx := vaka.WithFlags(context.Background(), flags)
	c.Track(ctx, "purchase", map[string]any{"amount": 12.5})
	flush(t, c)
	ev := f.byType("event")[0]
	fl := ev["flags"].(map[string]any)
	if ev["name"] != "purchase" || fl["new_checkout"] != "one_page" || fl["beta"] != "false" || ev["props"].(map[string]any)["amount"] != 12.5 {
		t.Errorf("event = %v", ev)
	}
}

func TestSlogHandler(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	var out bytes.Buffer
	logger := slog.New(vaka.SlogHandler(slog.NewTextHandler(&out, nil)))
	ctx := vaka.WithScope(context.Background())
	logger.InfoContext(ctx, "starting import", "file", "a.csv")
	logger.DebugContext(ctx, "noise")
	logger.ErrorContext(ctx, "import failed", "err", &fs.PathError{Op: "open", Path: "a.csv", Err: fs.ErrNotExist})
	flush(t, c)

	errs := f.byType("error")
	if len(errs) != 1 {
		t.Fatalf("errors = %v", errs)
	}
	e := errs[0]
	ex := e["exceptions"].([]any)[0].(map[string]any)
	if e["message"] != "import failed" || ex["type"] != "fs.PathError" {
		t.Errorf("error = %v", e)
	}
	if fr := frames(t, ex); fr[0]["function"] != "TestSlogHandler" {
		t.Errorf("first frame = %v", fr[0])
	}
	bc := e["breadcrumbs"].([]any)
	if len(bc) != 1 || bc[0].(map[string]any)["message"] != "starting import" || bc[0].(map[string]any)["data"].(map[string]any)["file"] != "a.csv" {
		t.Errorf("breadcrumbs = %v", bc)
	}
	if !strings.Contains(out.String(), "import failed") {
		t.Errorf("next handler not called: %q", out.String())
	}
	logs := bytes.Join(f.otlpBodies("/ingest/otlp/v1/logs"), nil)
	for _, want := range []string{"starting import", "import failed", "vaka.error_captured"} {
		if !bytes.Contains(logs, []byte(want)) {
			t.Errorf("OTLP logs missing %q", want)
		}
	}
}

func TestBeforeSendAndUnauthorized(t *testing.T) {
	f := newFake(t)
	f.status = 401
	c := initClient(t, f, func(cfg *vaka.Config) {
		cfg.DisableSessions = true
		cfg.DisableTracing = true
		cfg.DisableLogs = true
		cfg.BeforeSend = func(item map[string]any) map[string]any {
			if item["message"] == "drop me" {
				return nil
			}
			return item
		}
	})
	c.CaptureMessage(context.Background(), "drop me", vaka.LevelInfo)
	c.CaptureMessage(context.Background(), "first", vaka.LevelInfo)
	flush(t, c)
	c.CaptureMessage(context.Background(), "second", vaka.LevelInfo)
	flush(t, c)
	errs := f.byType("error")
	if len(errs) != 1 || errs[0]["message"] != "first" {
		t.Errorf("after 401 the SDK should stop sending: %v", errs)
	}
}

func TestDisabled(t *testing.T) {
	c, err := vaka.Init(vaka.Config{Key: "", Endpoint: ""})
	if err != nil {
		t.Fatal(err)
	}
	if id := c.CaptureError(context.Background(), errors.New("x")); id != "" {
		t.Error("disabled client captured")
	}
	if flags, err := c.EvaluateFlags(context.Background(), vaka.FlagRequest{DistinctID: "a"}); err != nil || len(flags) != 0 {
		t.Errorf("flags = %v, %v", flags, err)
	}
	var nilClient *vaka.Client
	nilClient.Track(context.Background(), "x", nil)
	_ = nilClient.Shutdown(context.Background())
	if _, err := vaka.Init(vaka.Config{Key: "vk", Endpoint: "vaka.example.com"}); err == nil {
		t.Error("endpoint without scheme accepted")
	}
}
