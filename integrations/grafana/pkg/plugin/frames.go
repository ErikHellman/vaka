package plugin

import (
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/grafana/grafana-plugin-sdk-go/data"
)

// Row is one JSON object from a Vaka response.
type Row = map[string]any

// seriesResponse is the shape every Vaka .../timeseries endpoint returns.
type seriesResponse struct {
	Interval string `json:"interval"`
	GroupBy  string `json:"group_by"`
	Series   []Row  `json:"series"`
	Groups   []struct {
		Key    string  `json:"key"`
		Title  string  `json:"title"`
		Total  float64 `json:"total"`
		Series []Row   `json:"series"`
	} `json:"groups"`
}

func parseTime(v any) (time.Time, bool) {
	s, ok := v.(string)
	if !ok || s == "" {
		return time.Time{}, false
	}
	for _, layout := range []string{time.RFC3339Nano, "2006-01-02 15:04:05.000", "2006-01-02 15:04:05", "2006-01-02"} {
		if t, err := time.Parse(layout, s); err == nil {
			return t.UTC(), true
		}
	}
	return time.Time{}, false
}

func num(v any) *float64 {
	switch x := v.(type) {
	case float64:
		return &x
	case int:
		f := float64(x)
		return &f
	case json.Number:
		if f, err := x.Float64(); err == nil {
			return &f
		}
	case bool:
		f := 0.0
		if x {
			f = 1
		}
		return &f
	}
	return nil
}

func str(v any) string {
	switch x := v.(type) {
	case nil:
		return ""
	case string:
		return x
	case float64:
		if x == float64(int64(x)) {
			return fmt.Sprintf("%d", int64(x))
		}
		return fmt.Sprintf("%g", x)
	default:
		b, _ := json.Marshal(x)
		return string(b)
	}
}

// metricConfig gives a field a unit and a readable name.
func metricConfig(metric string) *data.FieldConfig {
	cfg := &data.FieldConfig{}
	switch {
	case strings.HasSuffix(metric, "_ms"):
		cfg.Unit = "ms"
	case metric == "error_rate" || strings.HasPrefix(metric, "crash_free") || strings.HasPrefix(metric, "share_") ||
		strings.HasSuffix(metric, "_share") || strings.HasPrefix(metric, "anr_rate") || strings.HasPrefix(metric, "current_share") ||
		strings.HasPrefix(metric, "peak_share"):
		cfg.Unit = "percentunit"
	case metric == "per_minute":
		cfg.Unit = "reqpm"
	case strings.HasSuffix(metric, "_pct"):
		cfg.Unit = "percent"
	case strings.HasSuffix(metric, "_seconds"):
		cfg.Unit = "s"
	}
	return cfg
}

// timeField builds the time column from each row's key (usually "bucket").
func timeField(rows []Row, key string) *data.Field {
	ts := make([]time.Time, len(rows))
	for i, r := range rows {
		ts[i], _ = parseTime(r[key])
	}
	return data.NewField("time", nil, ts)
}

func numberField(name string, labels data.Labels, rows []Row, key string) *data.Field {
	vals := make([]*float64, len(rows))
	for i, r := range rows {
		vals[i] = num(r[key])
	}
	f := data.NewField(name, labels, vals)
	f.Config = metricConfig(key)
	return f
}

// seriesFrames turns a time series response into one wide frame: a time column and one number
// column per metric, or, with groups, one column per group for the first metric.
func seriesFrames(refID string, res seriesResponse, metrics []string) data.Frames {
	if len(res.Groups) > 0 {
		metric := metrics[0]
		// Groups share the bucket list only with fill=true; join them on time to be safe.
		times := map[time.Time]bool{}
		for _, g := range res.Groups {
			for _, r := range g.Series {
				if t, ok := parseTime(r["bucket"]); ok {
					times[t] = true
				}
			}
		}
		axis := make([]time.Time, 0, len(times))
		for t := range times {
			axis = append(axis, t)
		}
		sort.Slice(axis, func(i, j int) bool { return axis[i].Before(axis[j]) })
		index := make(map[time.Time]int, len(axis))
		for i, t := range axis {
			index[t] = i
		}
		frame := data.NewFrame(refID, data.NewField("time", nil, axis))
		for _, g := range res.Groups {
			vals := make([]*float64, len(axis))
			for _, r := range g.Series {
				if t, ok := parseTime(r["bucket"]); ok {
					vals[index[t]] = num(r[metric])
				}
			}
			f := data.NewField(metric, data.Labels{res.GroupBy: g.Key}, vals)
			f.Config = metricConfig(metric)
			name := g.Key
			if name == "" {
				name = "(none)"
			}
			if g.Title != "" {
				name += " " + g.Title
			}
			f.Config.DisplayNameFromDS = name
			frame.Fields = append(frame.Fields, f)
		}
		return data.Frames{frame}
	}
	frame := data.NewFrame(refID, timeField(res.Series, "bucket"))
	for _, m := range metrics {
		frame.Fields = append(frame.Fields, numberField(m, nil, res.Series, m))
	}
	return data.Frames{frame}
}

var timeKeys = map[string]bool{"timestamp": true, "time": true, "start": true, "first_seen": true, "last_seen": true, "first_session": true, "last_session": true, "deadline": true, "minute": true}

func isTimeKey(k string) bool {
	return timeKeys[k] || strings.HasSuffix(k, "_at") || strings.HasSuffix(k, "_since") || strings.HasSuffix(k, "_until")
}

// tableFrame builds a frame from rows. cols fixes the column order; nil means every key,
// alphabetically. Columns are typed from their values: numbers, booleans, times (by key
// name) and strings (objects and arrays become JSON).
func tableFrame(name string, rows []Row, cols []string) *data.Frame {
	if cols == nil {
		seen := map[string]bool{}
		for _, r := range rows {
			for k := range r {
				if !seen[k] {
					seen[k] = true
					cols = append(cols, k)
				}
			}
		}
		sort.Strings(cols)
	}
	frame := data.NewFrame(name)
	for _, c := range cols {
		kind := "string"
	scan:
		for _, r := range rows {
			v, ok := r[c]
			if !ok || v == nil {
				continue
			}
			switch v.(type) {
			case float64:
				kind = "number"
			case bool:
				kind = "bool"
			case string:
				if _, ok := parseTime(v); ok && isTimeKey(c) {
					kind = "time"
				}
			}
			break scan
		}
		switch kind {
		case "number":
			frame.Fields = append(frame.Fields, numberField(c, nil, rows, c))
		case "bool":
			vals := make([]*bool, len(rows))
			for i, r := range rows {
				if b, ok := r[c].(bool); ok {
					vals[i] = &b
				}
			}
			frame.Fields = append(frame.Fields, data.NewField(c, nil, vals))
		case "time":
			vals := make([]*time.Time, len(rows))
			for i, r := range rows {
				if t, ok := parseTime(r[c]); ok {
					vals[i] = &t
				}
			}
			frame.Fields = append(frame.Fields, data.NewField(c, nil, vals))
		default:
			vals := make([]string, len(rows))
			for i, r := range rows {
				vals[i] = str(r[c])
			}
			frame.Fields = append(frame.Fields, data.NewField(c, nil, vals))
		}
	}
	return frame
}

// singleRow turns one object into a one-row frame with the given keys in order.
func singleRow(name string, obj Row, keys []string) *data.Frame {
	return tableFrame(name, []Row{obj}, keys)
}

// field returns the frame's field with that name, or nil.
func field(f *data.Frame, name string) *data.Field {
	for _, fl := range f.Fields {
		if fl.Name == name {
			return fl
		}
	}
	return nil
}

// linkField adds a data link to a field.
func linkField(f *data.Frame, name, title, url string, targetBlank bool) {
	if fl := field(f, name); fl != nil {
		if fl.Config == nil {
			fl.Config = &data.FieldConfig{}
		}
		fl.Config.Links = append(fl.Config.Links, data.DataLink{Title: title, URL: url, TargetBlank: targetBlank})
	}
}

func toRawJSON(vals []string) []json.RawMessage {
	out := make([]json.RawMessage, len(vals))
	for i, v := range vals {
		if v == "" || v == "null" {
			v = "{}"
		}
		out[i] = json.RawMessage(v)
	}
	return out
}
