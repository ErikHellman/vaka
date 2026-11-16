package plugin

import (
	"fmt"
	"net/url"
	"strings"
	"time"

	"github.com/grafana/grafana-plugin-sdk-go/backend"
)

// Query types. Keep in sync with src/types.ts.
const (
	QTAnalyticsTimeseries = "analytics_timeseries"
	QTAnalyticsBreakdown  = "analytics_breakdown"
	QTAnalyticsSummary    = "analytics_summary"
	QTRealtime            = "realtime"
	QTErrorsTimeseries    = "errors_timeseries"
	QTIssues              = "issues"
	QTSessionsTimeseries  = "sessions_timeseries"
	QTReleaseHealth       = "release_health"
	QTReleaseAdoption     = "release_adoption"
	QTPerfTimeseries      = "performance_timeseries"
	QTPerfSummary         = "performance_summary"
	QTWebVitals           = "web_vitals"
	QTLogs                = "logs"
	QTTrace               = "trace"
	QTFunnel              = "funnel"
	QTMonitors            = "monitors"
	QTIncidents           = "incidents"
	QTLive                = "live"

	// Variable queries.
	QTVarProjects        = "var_projects"
	QTVarReleases        = "var_releases"
	QTVarDimensionValues = "var_dimension_values"
	QTVarEventNames      = "var_event_names"

	// Annotation queries.
	QTAnnReleases  = "ann_releases"
	QTAnnIncidents = "ann_incidents"
	QTAnnIssues    = "ann_issues"
)

// Filter is one analytics filter row: dimension, operator and value.
type Filter struct {
	Key   string `json:"key"`
	Op    string `json:"op"` // "=" (default), "!=", "=~" (contains)
	Value string `json:"value"`
}

// Query is the JSON model the query editor saves.
type Query struct {
	QueryType string `json:"queryType"`
	Project   string `json:"project"`

	// Time series options.
	Metrics  []string `json:"metrics"`
	GroupBy  string   `json:"groupBy"`
	Limit    int      `json:"limit"`
	Interval string   `json:"bucket"` // explicit bucket size, overrides the panel interval

	// Analytics.
	Dimension string   `json:"dimension"`
	EventType string   `json:"eventType"`
	Filters   []Filter `json:"filters"`
	Steps     []string `json:"steps"`
	Window    string   `json:"window"`

	// Errors, issues, releases, performance.
	Issue         string `json:"issue"`
	Level         string `json:"level"`
	Release       string `json:"release"`
	Environment   string `json:"environment"`
	ExceptionType string `json:"exceptionType"`
	Handled       string `json:"handled"`
	Status        string `json:"status"`
	Sort          string `json:"sort"`
	Search        string `json:"search"`
	Op            string `json:"op"`
	Transaction   string `json:"transaction"`
	ByPage        bool   `json:"byPage"`

	// Logs and traces.
	Severity string `json:"severity"`
	Service  string `json:"service"`
	TraceID  string `json:"traceId"`

	// Live.
	Stream string `json:"stream"` // errors | events | spans
}

func (q *Query) setFilters(v url.Values) {
	for _, f := range q.Filters {
		if strings.TrimSpace(f.Key) == "" {
			continue
		}
		val := f.Value
		switch f.Op {
		case "!=":
			val = "!" + val
		case "=~":
			val = "~" + val
		}
		v.Add("filter", f.Key+":"+val)
	}
}

func setIf(v url.Values, key, val string) {
	if s := strings.TrimSpace(val); s != "" {
		v.Set(key, s)
	}
}

// rangeParams sets since/until (unix ms) from the panel time range.
func rangeParams(v url.Values, tr backend.TimeRange) {
	v.Set("since", fmt.Sprintf("%d", tr.From.UnixMilli()))
	v.Set("until", fmt.Sprintf("%d", tr.To.UnixMilli()))
}

// Bucket sizes the plugin asks for, smallest first.
var niceSteps = []time.Duration{
	time.Minute, 2 * time.Minute, 5 * time.Minute, 10 * time.Minute, 15 * time.Minute, 30 * time.Minute,
	time.Hour, 2 * time.Hour, 3 * time.Hour, 6 * time.Hour, 12 * time.Hour, 24 * time.Hour, 7 * 24 * time.Hour,
}

// maxBuckets stays below the server's limit of 2000.
const maxBuckets = 1500

// pickInterval chooses the bucket size for a time series query: the explicit one if set,
// otherwise the panel's interval rounded up to a nice step, never more than maxBuckets.
func pickInterval(q Query, dq backend.DataQuery) string {
	if s := strings.TrimSpace(q.Interval); s != "" && s != "auto" {
		return s
	}
	span := dq.TimeRange.To.Sub(dq.TimeRange.From)
	target := dq.Interval
	if dq.MaxDataPoints > 0 {
		if t := span / time.Duration(dq.MaxDataPoints); t > target {
			target = t
		}
	}
	if t := span / maxBuckets; t > target {
		target = t
	}
	for _, s := range niceSteps {
		if s >= target {
			return formatStep(s)
		}
	}
	return "1w"
}

func formatStep(d time.Duration) string {
	switch {
	case d == 7*24*time.Hour:
		return "1w"
	case d%(24*time.Hour) == 0:
		return fmt.Sprintf("%dd", d/(24*time.Hour))
	case d%time.Hour == 0:
		return fmt.Sprintf("%dh", d/time.Hour)
	default:
		return fmt.Sprintf("%dm", d/time.Minute)
	}
}
