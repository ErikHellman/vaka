package plugin

import (
	"context"
	"fmt"
	"net/url"
	"sort"
	"strings"
	"time"

	"github.com/grafana/grafana-plugin-sdk-go/backend"
	"github.com/grafana/grafana-plugin-sdk-go/data"
)

func proj(q Query) string { return url.PathEscape(q.Project) }

func defaultMetrics(q Query, def ...string) []string {
	var out []string
	for _, m := range q.Metrics {
		if s := strings.TrimSpace(m); s != "" {
			out = append(out, s)
		}
	}
	if len(out) == 0 {
		return def
	}
	return out
}

func seriesParams(q Query, dq backend.DataQuery) url.Values {
	v := url.Values{}
	rangeParams(v, dq.TimeRange)
	v.Set("interval", pickInterval(q, dq))
	v.Set("fill", "true")
	setIf(v, "group_by", q.GroupBy)
	if q.GroupBy != "" && q.Limit > 0 {
		v.Set("limit", fmt.Sprint(q.Limit))
	}
	return v
}

func (d *Datasource) series(ctx context.Context, path string, v url.Values, refID string, metrics []string) (data.Frames, error) {
	var res seriesResponse
	if err := d.client.Get(ctx, path, v, &res); err != nil {
		return nil, err
	}
	return seriesFrames(refID, res, metrics), nil
}

func (d *Datasource) analyticsTimeseries(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := seriesParams(q, dq)
	setIf(v, "type", q.EventType)
	q.setFilters(v)
	return d.series(ctx, "projects/"+proj(q)+"/analytics/timeseries", v, dq.RefID, defaultMetrics(q, "visitors", "pageviews"))
}

func (d *Datasource) errorsTimeseries(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := seriesParams(q, dq)
	setIf(v, "issue", q.Issue)
	setIf(v, "level", q.Level)
	setIf(v, "release", q.Release)
	setIf(v, "environment", q.Environment)
	setIf(v, "exception_type", q.ExceptionType)
	setIf(v, "handled", q.Handled)
	return d.series(ctx, "projects/"+proj(q)+"/errors/timeseries", v, dq.RefID, defaultMetrics(q, "errors"))
}

func (d *Datasource) perfTimeseries(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := seriesParams(q, dq)
	setIf(v, "op", q.Op)
	setIf(v, "transaction", q.Transaction)
	setIf(v, "release", q.Release)
	setIf(v, "environment", q.Environment)
	return d.series(ctx, "projects/"+proj(q)+"/performance/timeseries", v, dq.RefID, defaultMetrics(q, "p50_ms", "p95_ms"))
}

func (d *Datasource) sessionsTimeseries(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := seriesParams(q, dq)
	setIf(v, "release", q.Release)
	setIf(v, "environment", q.Environment)
	return d.series(ctx, "projects/"+proj(q)+"/sessions/timeseries", v, dq.RefID, defaultMetrics(q, "crash_free_sessions"))
}

func (d *Datasource) analyticsBreakdown(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	if q.Dimension == "" {
		return nil, fmt.Errorf("choose a dimension")
	}
	v := url.Values{}
	rangeParams(v, dq.TimeRange)
	v.Set("dimension", q.Dimension)
	setIf(v, "type", q.EventType)
	if q.Limit > 0 {
		v.Set("limit", fmt.Sprint(q.Limit))
	}
	q.setFilters(v)
	var res struct {
		Rows []Row `json:"rows"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/analytics/breakdown", v, &res); err != nil {
		return nil, err
	}
	f := tableFrame(dq.RefID, res.Rows, append([]string{"value"}, defaultMetrics(q, "visitors", "pageviews", "events")...))
	if fl := field(f, "value"); fl != nil {
		fl.Name = q.Dimension
	}
	return data.Frames{f}, nil
}

var analyticsMetrics = []string{"visitors", "pageviews", "screen_views", "custom_events", "events", "identified_users"}

func (d *Datasource) analyticsSummary(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := url.Values{}
	rangeParams(v, dq.TimeRange)
	setIf(v, "type", q.EventType)
	q.setFilters(v)
	var res struct {
		Totals   Row `json:"totals"`
		Previous Row `json:"previous_period"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/analytics/summary", v, &res); err != nil {
		return nil, err
	}
	metrics := defaultMetrics(q, analyticsMetrics...)
	row := Row{}
	var cols []string
	for _, m := range metrics {
		row[m] = res.Totals[m]
		cols = append(cols, m)
	}
	for _, m := range metrics {
		row["previous_"+m] = res.Previous[m]
		cols = append(cols, "previous_"+m)
	}
	return data.Frames{singleRow(dq.RefID, row, cols)}, nil
}

func (d *Datasource) realtime(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := url.Values{}
	if q.Window != "" {
		v.Set("minutes", strings.TrimSuffix(q.Window, "m"))
	}
	var res Row
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/analytics/realtime", v, &res); err != nil {
		return nil, err
	}
	stats := singleRow(dq.RefID, res, []string{"visitors", "pageviews", "screen_views", "events", "active_sessions", "errors", "issues"})
	var perMin []Row
	if list, ok := res["per_minute_last_30"].([]any); ok {
		for _, x := range list {
			if r, ok := x.(Row); ok {
				perMin = append(perMin, r)
			}
		}
	}
	ts := data.NewFrame(dq.RefID+"-per-minute", timeField(perMin, "minute"), numberField("visitors", nil, perMin, "visitors"), numberField("events", nil, perMin, "events"))
	return data.Frames{stats, ts}, nil
}

func (d *Datasource) issues(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := url.Values{}
	v.Set("since", fmt.Sprint(dq.TimeRange.From.UnixMilli()))
	setIf(v, "status", q.Status)
	setIf(v, "sort", q.Sort)
	setIf(v, "query", q.Search)
	setIf(v, "release", q.Release)
	setIf(v, "environment", q.Environment)
	limit := q.Limit
	if limit <= 0 {
		limit = 50
	}
	v.Set("limit", fmt.Sprint(limit))
	var res struct {
		Issues []Row `json:"issues"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/issues", v, &res); err != nil {
		return nil, err
	}
	f := tableFrame(dq.RefID, res.Issues, []string{"short_id", "title", "level", "status", "events_in_window", "users_in_window", "times_seen", "first_seen", "last_seen", "first_release", "last_release", "culprit", "assignee", "url", "id"})
	linkField(f, "short_id", "Open in Vaka", "${__data.fields.url}", true)
	linkField(f, "title", "Open in Vaka", "${__data.fields.url}", true)
	return data.Frames{f}, nil
}

func (d *Datasource) releaseHealth(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := url.Values{}
	v.Set("since", fmt.Sprint(dq.TimeRange.From.UnixMilli()))
	setIf(v, "release", q.Release)
	var res struct {
		Releases []Row `json:"releases"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/releases/health", v, &res); err != nil {
		return nil, err
	}
	return data.Frames{tableFrame(dq.RefID, res.Releases, []string{"release", "sessions", "users", "crash_free_sessions", "crash_free_users", "crashed", "errored", "errors", "new_issues", "issues_seen", "anrs", "anr_rate_sessions", "avg_session_seconds", "first_session", "last_session"})}, nil
}

func (d *Datasource) releaseAdoption(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := url.Values{}
	v.Set("since", fmt.Sprint(dq.TimeRange.From.UnixMilli()))
	if dq.TimeRange.To.Sub(dq.TimeRange.From) <= 3*24*time.Hour {
		v.Set("interval", "hour")
	} else {
		v.Set("interval", "day")
	}
	setIf(v, "environment", q.Environment)
	if q.Limit > 0 {
		v.Set("limit", fmt.Sprint(q.Limit))
	}
	var res struct {
		Series []struct {
			T        string         `json:"t"`
			Releases map[string]Row `json:"releases"`
		} `json:"series"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/releases/adoption", v, &res); err != nil {
		return nil, err
	}
	metric := defaultMetrics(q, "share_sessions")[0]
	names := map[string]bool{}
	for _, s := range res.Series {
		for r := range s.Releases {
			names[r] = true
		}
	}
	releases := make([]string, 0, len(names))
	for r := range names {
		releases = append(releases, r)
	}
	sort.Strings(releases)
	times := make([]time.Time, len(res.Series))
	for i, s := range res.Series {
		times[i], _ = parseTime(s.T)
	}
	frame := data.NewFrame(dq.RefID, data.NewField("time", nil, times))
	for _, rel := range releases {
		vals := make([]*float64, len(res.Series))
		for i, s := range res.Series {
			if r, ok := s.Releases[rel]; ok {
				vals[i] = num(r[metric])
			} else {
				zero := 0.0
				vals[i] = &zero
			}
		}
		f := data.NewField(metric, data.Labels{"release": rel}, vals)
		f.Config = metricConfig(metric)
		f.Config.DisplayNameFromDS = rel
		frame.Fields = append(frame.Fields, f)
	}
	return data.Frames{frame}, nil
}

func perfParams(q Query, dq backend.DataQuery) url.Values {
	v := url.Values{}
	rangeParams(v, dq.TimeRange)
	setIf(v, "op", q.Op)
	setIf(v, "transaction", q.Transaction)
	setIf(v, "release", q.Release)
	setIf(v, "environment", q.Environment)
	setIf(v, "sort", q.Sort)
	if q.Limit > 0 {
		v.Set("limit", fmt.Sprint(q.Limit))
	}
	return v
}

func (d *Datasource) perfSummary(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	var res struct {
		Transactions []Row `json:"transactions"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/performance/summary", perfParams(q, dq), &res); err != nil {
		return nil, err
	}
	return data.Frames{tableFrame(dq.RefID, res.Transactions, []string{"transaction", "op", "count", "per_minute", "p50_ms", "p75_ms", "p95_ms", "p99_ms", "avg_ms", "max_ms", "error_rate"})}, nil
}

func (d *Datasource) webVitals(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := perfParams(q, dq)
	v.Del("op")
	if q.ByPage {
		v.Set("by_page", "true")
	}
	var res struct {
		Overall []Row `json:"overall"`
		Pages   []struct {
			Metrics []Row `json:"metrics"`
		} `json:"pages"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/performance/web-vitals", v, &res); err != nil {
		return nil, err
	}
	cols := []string{"metric", "p75", "p50", "rating", "good_share", "poor_share", "samples"}
	if q.ByPage {
		var rows []Row
		for _, p := range res.Pages {
			rows = append(rows, p.Metrics...)
		}
		return data.Frames{tableFrame(dq.RefID, rows, append([]string{"page"}, cols...))}, nil
	}
	return data.Frames{tableFrame(dq.RefID, res.Overall, cols)}, nil
}

func (d *Datasource) funnel(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := url.Values{}
	rangeParams(v, dq.TimeRange)
	n := 0
	for _, s := range q.Steps {
		if s = strings.TrimSpace(s); s != "" {
			v.Add("step", s)
			n++
		}
	}
	if n < 2 {
		return nil, fmt.Errorf("a funnel needs at least two steps")
	}
	setIf(v, "window", q.Window)
	q.setFilters(v)
	var res struct {
		Steps []Row `json:"steps"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/analytics/funnel", v, &res); err != nil {
		return nil, err
	}
	return data.Frames{tableFrame(dq.RefID, res.Steps, []string{"step", "label", "count", "conversion_from_start_pct", "conversion_from_previous_pct", "dropped_from_previous"})}, nil
}

func (d *Datasource) monitors(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	var res struct {
		Monitors []Row `json:"monitors"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/monitors", nil, &res); err != nil {
		return nil, err
	}
	rows := make([]Row, 0, len(res.Monitors))
	for _, m := range res.Monitors {
		r := Row{}
		for k, val := range m {
			r[k] = val
		}
		if st, ok := m["stats"].(Row); ok {
			for _, k := range []string{"uptime_24h", "uptime_7d", "uptime_30d", "avg_duration_ms"} {
				r[k] = st[k]
			}
		}
		// The API reports uptime in percent; 1 when up, 0 otherwise is handy for alerting.
		up := 0.0
		if m["state"] == "up" {
			up = 1
		}
		r["up"] = up
		rows = append(rows, r)
	}
	f := tableFrame(dq.RefID, rows, []string{"name", "slug", "kind", "state", "up", "uptime_24h", "uptime_7d", "uptime_30d", "avg_duration_ms", "last_check_at", "last_error", "environment", "state_since"})
	for _, k := range []string{"uptime_24h", "uptime_7d", "uptime_30d"} {
		if fl := field(f, k); fl != nil {
			fl.Config.Unit = "percent"
		}
	}
	return data.Frames{f}, nil
}

func (d *Datasource) incidentRows(ctx context.Context, q Query, dq backend.DataQuery) ([]Row, error) {
	v := url.Values{}
	v.Set("since", fmt.Sprint(dq.TimeRange.From.UnixMilli()))
	v.Set("status", "all")
	v.Set("limit", "200")
	var res struct {
		Incidents []Row `json:"incidents"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/incidents", v, &res); err != nil {
		return nil, err
	}
	for _, r := range res.Incidents {
		if m, ok := r["monitor"].(Row); ok {
			r["monitor_name"] = m["name"]
			r["monitor_slug"] = m["slug"]
		}
	}
	return res.Incidents, nil
}

func (d *Datasource) incidents(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	rows, err := d.incidentRows(ctx, q, dq)
	if err != nil {
		return nil, err
	}
	return data.Frames{tableFrame(dq.RefID, rows, []string{"started_at", "ended_at", "monitor_name", "kind", "open", "failed_checks", "first_error", "acknowledged_at", "id"})}, nil
}

func (d *Datasource) logs(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	v := url.Values{}
	rangeParams(v, dq.TimeRange)
	setIf(v, "q", q.Search)
	setIf(v, "severity", q.Severity)
	setIf(v, "service", q.Service)
	setIf(v, "trace_id", q.TraceID)
	setIf(v, "release", q.Release)
	setIf(v, "environment", q.Environment)
	limit := q.Limit
	if limit <= 0 {
		limit = 500
	}
	v.Set("limit", fmt.Sprint(limit))
	var res struct {
		Logs []Row `json:"logs"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/logs", v, &res); err != nil {
		return nil, err
	}
	return data.Frames{logsFrame(dq.RefID, res.Logs)}, nil
}

// logsFrame follows Grafana's logs data frame format (timestamp, body, severity, labels).
func logsFrame(refID string, logs []Row) *data.Frame {
	n := len(logs)
	ts := make([]time.Time, n)
	body := make([]string, n)
	sev := make([]string, n)
	traceIDs := make([]string, n)
	labelVals := make([]string, n)
	for i, l := range logs {
		ts[i], _ = parseTime(l["timestamp"])
		body[i] = str(l["body"])
		sev[i] = str(l["severity"])
		traceIDs[i] = str(l["trace_id"])
		lb := map[string]string{}
		for _, k := range []string{"service", "release", "environment", "span_id"} {
			if s := str(l[k]); s != "" {
				lb[k] = s
			}
		}
		if attrs, ok := l["attributes"].(Row); ok {
			for k, val := range attrs {
				lb[k] = str(val)
			}
		}
		labelVals[i] = str(lb)
	}
	f := data.NewFrame(refID,
		data.NewField("timestamp", nil, ts),
		data.NewField("body", nil, body),
		data.NewField("severity", nil, sev),
		data.NewField("traceID", nil, traceIDs),
		data.NewField("labels", nil, toRawJSON(labelVals)),
	)
	f.SetMeta(&data.FrameMeta{PreferredVisualization: data.VisTypeLogs, Type: data.FrameTypeLogLines, TypeVersion: data.FrameTypeVersion{0, 0}})
	return f
}

func (d *Datasource) trace(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	id := strings.TrimSpace(q.TraceID)
	if id == "" {
		return nil, fmt.Errorf("enter a trace id")
	}
	var res struct {
		TraceID string `json:"trace_id"`
		Start   string `json:"start"`
		Spans   []Row  `json:"spans"`
	}
	if err := d.client.Get(ctx, "projects/"+proj(q)+"/traces/"+url.PathEscape(id), nil, &res); err != nil {
		return nil, err
	}
	return data.Frames{traceFrame(dq.RefID, res.TraceID, res.Start, res.Spans)}, nil
}

// traceFrame follows Grafana's trace data frame format, rendered by the trace view.
func traceFrame(refID, traceID, start string, spans []Row) *data.Frame {
	t0, _ := parseTime(start)
	base := float64(t0.UnixMicro()) / 1000
	n := len(spans)
	tid, sid, pid, opName, svc, kind := make([]string, n), make([]string, n), make([]string, n), make([]string, n), make([]string, n), make([]string, n)
	status := make([]int64, n)
	startMs, dur := make([]float64, n), make([]float64, n)
	tags := make([]string, n)
	svcTags := make([]string, n)
	for i, s := range spans {
		tid[i] = traceID
		sid[i] = str(s["span_id"])
		pid[i] = str(s["parent_span_id"])
		opName[i] = str(s["name"])
		svc[i] = str(s["service"])
		if svc[i] == "" {
			svc[i] = str(s["op"])
		}
		if f := num(s["offset_ms"]); f != nil {
			startMs[i] = base + *f
		}
		if f := num(s["duration_ms"]); f != nil {
			dur[i] = *f
		}
		var tg []map[string]any
		for _, k := range []string{"op", "release", "http_method", "http_status", "status", "self_time_ms"} {
			if v, ok := s[k]; ok && v != nil && str(v) != "" {
				tg = append(tg, map[string]any{"key": k, "value": v})
			}
		}
		tags[i] = "[]"
		if len(tg) > 0 {
			tags[i] = str(tg)
		}
		svcTags[i] = "[]"
		// OpenTelemetry status codes: 0 unset, 1 ok, 2 error.
		switch str(s["status"]) {
		case "error":
			status[i] = 2
		case "ok":
			status[i] = 1
		}
		kind[i] = "server"
		if pid[i] != "" {
			kind[i] = "internal"
		}
	}
	f := data.NewFrame(refID,
		data.NewField("traceID", nil, tid),
		data.NewField("spanID", nil, sid),
		data.NewField("parentSpanID", nil, pid),
		data.NewField("operationName", nil, opName),
		data.NewField("serviceName", nil, svc),
		data.NewField("serviceTags", nil, toRawJSON(svcTags)),
		data.NewField("startTime", nil, startMs),
		data.NewField("duration", nil, dur),
		data.NewField("tags", nil, toRawJSON(tags)),
		data.NewField("kind", nil, kind),
		data.NewField("statusCode", nil, status),
	)
	f.SetMeta(&data.FrameMeta{PreferredVisualization: data.VisTypeTrace})
	return f
}
