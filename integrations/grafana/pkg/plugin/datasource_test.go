package plugin

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/ErikHellman/vaka/integrations/grafana/pkg/models"
	"github.com/ErikHellman/vaka/integrations/grafana/pkg/vaka"
	"github.com/grafana/grafana-plugin-sdk-go/backend"
	"github.com/grafana/grafana-plugin-sdk-go/data"
)

// fakeVaka serves canned JSON per path and records the last query string per path.
func fakeVaka(t *testing.T, routes map[string]string) (*Datasource, map[string]string) {
	t.Helper()
	seen := map[string]string{}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer vt_test" {
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = w.Write([]byte(`{"error":"bad token"}`))
			return
		}
		p := strings.TrimPrefix(r.URL.Path, "/api/v1/")
		seen[p] = r.URL.RawQuery
		body, ok := routes[p]
		if !ok {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(body))
	}))
	t.Cleanup(srv.Close)
	c := &vaka.Client{BaseURL: srv.URL, Token: "vt_test", HTTP: srv.Client()}
	return &Datasource{settings: &models.PluginSettings{URL: srv.URL, Secrets: &models.SecretPluginSettings{Token: "vt_test"}}, client: c, stream: c}, seen
}

func run(t *testing.T, d *Datasource, q map[string]any) backend.DataResponse {
	t.Helper()
	raw, _ := json.Marshal(q)
	to := time.Date(2026, 11, 16, 12, 0, 0, 0, time.UTC)
	res, err := d.QueryData(context.Background(), &backend.QueryDataRequest{Queries: []backend.DataQuery{{
		RefID: "A", JSON: raw, Interval: time.Minute, MaxDataPoints: 500,
		TimeRange: backend.TimeRange{From: to.Add(-6 * time.Hour), To: to},
	}}})
	if err != nil {
		t.Fatal(err)
	}
	return res.Responses["A"]
}

func TestErrorsTimeseriesGrouped(t *testing.T) {
	d, seen := fakeVaka(t, map[string]string{"projects/web/errors/timeseries": `{
		"interval": "5m", "group_by": "issue",
		"series": [{"bucket": "2026-11-16T11:50:00Z", "errors": 3}, {"bucket": "2026-11-16T11:55:00Z", "errors": 1}],
		"groups": [
			{"key": "WEB-1", "title": "TypeError", "total": 3, "series": [{"bucket": "2026-11-16T11:50:00Z", "errors": 3}, {"bucket": "2026-11-16T11:55:00Z", "errors": 0}]},
			{"key": "WEB-2", "total": 1, "series": [{"bucket": "2026-11-16T11:55:00Z", "errors": 1}]}
		]}`})
	r := run(t, d, map[string]any{"queryType": "errors_timeseries", "project": "web", "groupBy": "issue", "level": "error"})
	if r.Error != nil {
		t.Fatal(r.Error)
	}
	q := seen["projects/web/errors/timeseries"]
	for _, want := range []string{"fill=true", "group_by=issue", "level=error", "interval=1m", "since=", "until="} {
		if !strings.Contains(q, want) {
			t.Errorf("query %q lacks %q", q, want)
		}
	}
	f := r.Frames[0]
	if len(f.Fields) != 3 || f.Rows() != 2 {
		t.Fatalf("want time + 2 groups x 2 rows, got %d fields %d rows", len(f.Fields), f.Rows())
	}
	if f.Fields[1].Labels["issue"] != "WEB-1" || f.Fields[1].Config.DisplayNameFromDS != "WEB-1 TypeError" {
		t.Errorf("labels/display: %v %q", f.Fields[1].Labels, f.Fields[1].Config.DisplayNameFromDS)
	}
	// WEB-2 has no point at 11:50.
	if v := f.Fields[2].At(0).(*float64); v != nil {
		t.Errorf("missing bucket should be null, got %v", v)
	}
}

func TestPerfTimeseriesUnits(t *testing.T) {
	d, _ := fakeVaka(t, map[string]string{"projects/web/performance/timeseries": `{"series": [{"bucket": "2026-11-16T11:00:00Z", "p95_ms": 120.5, "error_rate": null}]}`})
	r := run(t, d, map[string]any{"queryType": "performance_timeseries", "project": "web", "metrics": []string{"p95_ms", "error_rate"}})
	if r.Error != nil {
		t.Fatal(r.Error)
	}
	f := r.Frames[0]
	if f.Fields[1].Config.Unit != "ms" || f.Fields[2].Config.Unit != "percentunit" {
		t.Errorf("units: %q %q", f.Fields[1].Config.Unit, f.Fields[2].Config.Unit)
	}
}

func TestIssuesTable(t *testing.T) {
	d, seen := fakeVaka(t, map[string]string{"projects/web/issues": `{"issues": [{"short_id": "WEB-1", "title": "TypeError", "status": "open", "events_in_window": 4, "first_seen": "2026-11-16T10:00:00.5Z", "url": "https://v/ui/x", "assignee": null}]}`})
	r := run(t, d, map[string]any{"queryType": "issues", "project": "web", "status": "open", "search": "Type"})
	if r.Error != nil {
		t.Fatal(r.Error)
	}
	if !strings.Contains(seen["projects/web/issues"], "query=Type") {
		t.Errorf("search not passed: %s", seen["projects/web/issues"])
	}
	f := r.Frames[0]
	if fl := field(f, "first_seen"); fl == nil || fl.Type() != data.FieldTypeNullableTime {
		t.Errorf("first_seen should be a time field")
	}
	if fl := field(f, "events_in_window"); fl == nil || fl.Type() != data.FieldTypeNullableFloat64 {
		t.Errorf("events_in_window should be numeric")
	}
	if len(field(f, "short_id").Config.Links) != 1 {
		t.Errorf("short_id should link to Vaka")
	}
}

func TestAnalyticsFiltersAndBreakdown(t *testing.T) {
	d, seen := fakeVaka(t, map[string]string{"projects/web/analytics/breakdown": `{"rows": [{"value": "SE", "visitors": 5, "pageviews": 9, "events": 12}]}`})
	r := run(t, d, map[string]any{"queryType": "analytics_breakdown", "project": "web", "dimension": "country",
		"filters": []map[string]string{{"key": "path", "op": "=~", "value": "/pricing"}, {"key": "browser", "op": "!=", "value": "Safari"}}})
	if r.Error != nil {
		t.Fatal(r.Error)
	}
	q := seen["projects/web/analytics/breakdown"]
	if !strings.Contains(q, "filter=path%3A~%2Fpricing") || !strings.Contains(q, "filter=browser%3A%21Safari") {
		t.Errorf("filters: %s", q)
	}
	if r.Frames[0].Fields[0].Name != "country" {
		t.Errorf("first column should be named after the dimension")
	}
}

func TestErrorsAndUnknownType(t *testing.T) {
	d, _ := fakeVaka(t, map[string]string{})
	if r := run(t, d, map[string]any{"queryType": "nope", "project": "web"}); r.Error == nil {
		t.Error("unknown type should fail")
	}
	if r := run(t, d, map[string]any{"queryType": "issues"}); r.Error == nil || !strings.Contains(r.Error.Error(), "project") {
		t.Errorf("missing project: %v", r.Error)
	}
	r := run(t, d, map[string]any{"queryType": "errors_timeseries", "project": "web"})
	if r.Error == nil || !strings.Contains(r.Error.Error(), "too old") {
		t.Errorf("404 on a series endpoint should hint at an old server: %v", r.Error)
	}
	d.client.Token = "wrong"
	if r := run(t, d, map[string]any{"queryType": "issues", "project": "web"}); r.Status != backend.StatusUnauthorized {
		t.Errorf("want 401, got %v %v", r.Status, r.Error)
	}
}

func TestCheckHealth(t *testing.T) {
	d, _ := fakeVaka(t, map[string]string{
		"me":       `{"type": "token", "name": "grafana", "scope": "read", "server_version": "0.5.0", "capabilities": ["errors_timeseries", "stream"]}`,
		"projects": `{"projects": [{"slug": "web"}, {"slug": "app"}]}`,
	})
	res, _ := d.CheckHealth(context.Background(), &backend.CheckHealthRequest{})
	if res.Status != backend.HealthStatusOk || !strings.Contains(res.Message, "2 project") {
		t.Errorf("%v %s", res.Status, res.Message)
	}
	d.settings.Secrets.Token = ""
	if res, _ := d.CheckHealth(context.Background(), &backend.CheckHealthRequest{}); res.Status != backend.HealthStatusError {
		t.Error("missing token should fail")
	}
}

func TestAnnotationsIssues(t *testing.T) {
	d, _ := fakeVaka(t, map[string]string{"projects/web/issues": `{"issues": [
		{"short_id": "WEB-1", "title": "a", "first_seen": "2026-11-16T10:00:00Z", "regressed_at": "2026-11-16T11:00:00Z"},
		{"short_id": "WEB-2", "title": "old", "first_seen": "2026-11-01T10:00:00Z"}]}`})
	r := run(t, d, map[string]any{"queryType": "ann_issues", "project": "web"})
	if r.Error != nil {
		t.Fatal(r.Error)
	}
	if n := r.Frames[0].Rows(); n != 2 {
		t.Errorf("want new issue + regression in range, got %d", n)
	}
}

func TestPickInterval(t *testing.T) {
	to := time.Now()
	dq := func(span, interval time.Duration, mdp int64) backend.DataQuery {
		return backend.DataQuery{TimeRange: backend.TimeRange{From: to.Add(-span), To: to}, Interval: interval, MaxDataPoints: mdp}
	}
	cases := []struct {
		q    Query
		dq   backend.DataQuery
		want string
	}{
		{Query{}, dq(time.Hour, 15*time.Second, 1000), "1m"},
		{Query{}, dq(24*time.Hour, 2*time.Minute, 1000), "2m"},
		{Query{}, dq(7*24*time.Hour, time.Minute, 300), "1h"},
		{Query{}, dq(90*24*time.Hour, time.Minute, 2000), "2h"},
		{Query{Interval: "1d"}, dq(time.Hour, time.Minute, 100), "1d"},
		{Query{}, dq(5*365*24*time.Hour, time.Hour, 1000), "1w"},
	}
	for _, c := range cases {
		if got := pickInterval(c.q, c.dq); got != c.want {
			t.Errorf("span %v interval %v: got %s want %s", c.dq.TimeRange.To.Sub(c.dq.TimeRange.From), c.dq.Interval, got, c.want)
		}
	}
}

func TestLiveFrames(t *testing.T) {
	f, ok := liveEventFrame("errors", vaka.Event{Name: "error", Data: json.RawMessage(`{"timestamp": "2026-11-16T12:00:00Z", "exception_type": "TypeError", "exception_value": "x", "handled": false}`)})
	if !ok || field(f, "title") == nil {
		t.Fatal("error event should become a frame")
	}
	if v, _ := field(f, "title").ConcreteAt(0); v != "TypeError: x" {
		t.Errorf("title %v", v)
	}
	if _, ok := liveEventFrame("errors", vaka.Event{Name: "ready", Data: json.RawMessage(`{}`)}); ok {
		t.Error("ready event should be skipped")
	}
	if _, err := parseChannel("errors/web/production"); err != nil {
		t.Error(err)
	}
	if _, err := parseChannel("../web"); err == nil {
		t.Error("bad path accepted")
	}
}

func TestTraceAndLogsFrames(t *testing.T) {
	tf := traceFrame("A", "t1", "2026-11-16T12:00:00Z", []Row{
		{"span_id": "a", "parent_span_id": "", "name": "GET /", "op": "http.server", "offset_ms": 0.0, "duration_ms": 10.0, "status": "ok"},
		{"span_id": "b", "parent_span_id": "a", "name": "db", "op": "db", "offset_ms": 2.0, "duration_ms": 5.0, "status": "error"},
	})
	if tf.Meta.PreferredVisualization != data.VisTypeTrace || tf.Rows() != 2 {
		t.Error("trace frame")
	}
	lf := logsFrame("A", []Row{{"timestamp": "2026-11-16T12:00:00Z", "body": "hi", "severity": "info", "service": "api", "attributes": map[string]any{"k": "v"}}})
	if lf.Meta.Type != data.FrameTypeLogLines {
		t.Error("logs frame type")
	}
	if v, _ := field(lf, "labels").ConcreteAt(0); !strings.Contains(string(v.(json.RawMessage)), `"k":"v"`) {
		t.Errorf("labels %s", v)
	}
}

// TestLiveServer runs every query type against a real Vaka server seeded by scripts/smoke.sh:
// VAKA_TEST_URL=http://127.0.0.1:8080 VAKA_TEST_TOKEN=vt_... VAKA_TEST_PROJECT=smoke-123 go test ./pkg/...
func TestLiveServer(t *testing.T) {
	base, token, project := os.Getenv("VAKA_TEST_URL"), os.Getenv("VAKA_TEST_TOKEN"), os.Getenv("VAKA_TEST_PROJECT")
	if base == "" || token == "" || project == "" {
		t.Skip("VAKA_TEST_URL, VAKA_TEST_TOKEN and VAKA_TEST_PROJECT not set")
	}
	c := &vaka.Client{BaseURL: base, Token: token, HTTP: &http.Client{Timeout: 30 * time.Second}}
	d := &Datasource{settings: &models.PluginSettings{URL: base, Secrets: &models.SecretPluginSettings{Token: token}}, client: c, stream: c}
	if res, _ := d.CheckHealth(context.Background(), &backend.CheckHealthRequest{}); res.Status != backend.HealthStatusOk {
		t.Errorf("health: %s", res.Message)
	}
	to := time.Now()
	for _, q := range []map[string]any{
		{"queryType": "analytics_timeseries"}, {"queryType": "analytics_timeseries", "groupBy": "path"},
		{"queryType": "analytics_breakdown", "dimension": "path"}, {"queryType": "analytics_summary"}, {"queryType": "realtime"},
		{"queryType": "errors_timeseries"}, {"queryType": "errors_timeseries", "groupBy": "issue"}, {"queryType": "issues"},
		{"queryType": "sessions_timeseries", "groupBy": "release"}, {"queryType": "release_health"}, {"queryType": "release_adoption"},
		{"queryType": "performance_timeseries", "groupBy": "transaction"}, {"queryType": "performance_summary"}, {"queryType": "web_vitals"},
		{"queryType": "web_vitals", "byPage": true}, {"queryType": "logs"}, {"queryType": "funnel", "steps": []string{"pageview", "add_to_cart"}},
		{"queryType": "monitors"}, {"queryType": "incidents"}, {"queryType": "var_projects"}, {"queryType": "var_releases"},
		{"queryType": "var_event_names"}, {"queryType": "var_dimension_values", "dimension": "path"},
		{"queryType": "ann_releases"}, {"queryType": "ann_incidents"}, {"queryType": "ann_issues"}, {"queryType": "live", "stream": "errors"},
	} {
		q["project"] = project
		raw, _ := json.Marshal(q)
		res, _ := d.QueryData(context.Background(), &backend.QueryDataRequest{Queries: []backend.DataQuery{{
			RefID: "A", JSON: raw, Interval: time.Minute, MaxDataPoints: 800, TimeRange: backend.TimeRange{From: to.Add(-3 * time.Hour), To: to},
		}}})
		r := res.Responses["A"]
		if r.Error != nil {
			t.Errorf("%v: %v", q, r.Error)
			continue
		}
		rows := 0
		for _, f := range r.Frames {
			rows += f.Rows()
		}
		t.Logf("%-24v %v frames, %d rows", q["queryType"], len(r.Frames), rows)
	}
}
