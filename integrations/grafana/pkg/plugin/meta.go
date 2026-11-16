package plugin

import (
	"context"
	"fmt"
	"net/url"
	"sort"
	"time"

	"github.com/grafana/grafana-plugin-sdk-go/backend"
	"github.com/grafana/grafana-plugin-sdk-go/data"
)

// Option is a value for a dropdown or template variable.
type Option struct {
	Text  string `json:"text"`
	Value string `json:"value"`
}

func optionsFrame(refID string, opts []Option) *data.Frame {
	text := make([]string, len(opts))
	value := make([]string, len(opts))
	for i, o := range opts {
		text[i], value[i] = o.Text, o.Value
	}
	return data.NewFrame(refID, data.NewField("text", nil, text), data.NewField("value", nil, value))
}

func (d *Datasource) projectOptions(ctx context.Context) ([]Option, error) {
	var res struct {
		Projects []struct {
			Slug string `json:"slug"`
			Name string `json:"name"`
		} `json:"projects"`
	}
	if err := d.client.Get(ctx, "projects", nil, &res); err != nil {
		return nil, err
	}
	out := make([]Option, 0, len(res.Projects))
	for _, p := range res.Projects {
		text := p.Name
		if text == "" {
			text = p.Slug
		}
		out = append(out, Option{Text: text, Value: p.Slug})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Text < out[j].Text })
	return out, nil
}

func (d *Datasource) releaseOptions(ctx context.Context, project string) ([]Option, error) {
	if project == "" {
		return nil, fmt.Errorf("choose a project")
	}
	var res struct {
		Releases []struct {
			Version string `json:"version"`
		} `json:"releases"`
	}
	if err := d.client.Get(ctx, "projects/"+url.PathEscape(project)+"/releases", nil, &res); err != nil {
		return nil, err
	}
	out := make([]Option, 0, len(res.Releases))
	for _, r := range res.Releases {
		out = append(out, Option{Text: r.Version, Value: r.Version})
	}
	return out, nil
}

func (d *Datasource) eventNameOptions(ctx context.Context, project, since string) ([]Option, error) {
	if project == "" {
		return nil, fmt.Errorf("choose a project")
	}
	v := url.Values{}
	if since == "" {
		since = "30d"
	}
	v.Set("since", since)
	var res struct {
		Events []struct {
			Name     string   `json:"name"`
			PropKeys []string `json:"prop_keys"`
		} `json:"events"`
	}
	if err := d.client.Get(ctx, "projects/"+url.PathEscape(project)+"/analytics/events", v, &res); err != nil {
		return nil, err
	}
	out := make([]Option, 0, len(res.Events))
	for _, e := range res.Events {
		out = append(out, Option{Text: e.Name, Value: e.Name})
	}
	return out, nil
}

// propKeyOptions lists custom event property keys as prop:<key> dimensions.
func (d *Datasource) propKeyOptions(ctx context.Context, project string) ([]Option, error) {
	v := url.Values{}
	v.Set("since", "30d")
	var res struct {
		Events []struct {
			PropKeys []string `json:"prop_keys"`
		} `json:"events"`
	}
	if err := d.client.Get(ctx, "projects/"+url.PathEscape(project)+"/analytics/events", v, &res); err != nil {
		return nil, err
	}
	seen := map[string]bool{}
	var out []Option
	for _, e := range res.Events {
		for _, k := range e.PropKeys {
			if !seen[k] {
				seen[k] = true
				out = append(out, Option{Text: "prop:" + k, Value: "prop:" + k})
			}
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Text < out[j].Text })
	return out, nil
}

func (d *Datasource) dimensionValueOptions(ctx context.Context, project, dimension string, from, to time.Time) ([]Option, error) {
	if project == "" || dimension == "" {
		return nil, fmt.Errorf("choose a project and a dimension")
	}
	v := url.Values{}
	v.Set("since", fmt.Sprint(from.UnixMilli()))
	v.Set("until", fmt.Sprint(to.UnixMilli()))
	v.Set("dimension", dimension)
	v.Set("limit", "100")
	var res struct {
		Rows []Row `json:"rows"`
	}
	if err := d.client.Get(ctx, "projects/"+url.PathEscape(project)+"/analytics/breakdown", v, &res); err != nil {
		return nil, err
	}
	out := make([]Option, 0, len(res.Rows))
	for _, r := range res.Rows {
		s := str(r["value"])
		if s != "" {
			out = append(out, Option{Text: s, Value: s})
		}
	}
	return out, nil
}

func (d *Datasource) variableQuery(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	var opts []Option
	var err error
	switch q.QueryType {
	case QTVarProjects:
		opts, err = d.projectOptions(ctx)
	case QTVarReleases:
		opts, err = d.releaseOptions(ctx, q.Project)
	case QTVarEventNames:
		opts, err = d.eventNameOptions(ctx, q.Project, fmt.Sprint(dq.TimeRange.From.UnixMilli()))
	case QTVarDimensionValues:
		opts, err = d.dimensionValueOptions(ctx, q.Project, q.Dimension, dq.TimeRange.From, dq.TimeRange.To)
	}
	if err != nil {
		return nil, err
	}
	return data.Frames{optionsFrame(dq.RefID, opts)}, nil
}

// annotationFrame has the fields Grafana maps to annotations: time, timeEnd, title, text, tags.
func annotationFrame(refID string, times, ends []*time.Time, titles, texts []string, tags [][]string) *data.Frame {
	tagJSON := make([]string, len(tags))
	for i, t := range tags {
		tagJSON[i] = str(t)
		if len(t) == 0 {
			tagJSON[i] = "[]"
		}
	}
	f := data.NewFrame(refID,
		data.NewField("time", nil, times),
		data.NewField("timeEnd", nil, ends),
		data.NewField("title", nil, titles),
		data.NewField("text", nil, texts),
		data.NewField("tags", nil, toRawJSON(tagJSON)),
	)
	return f
}

func inRange(t time.Time, tr backend.TimeRange) bool {
	return !t.Before(tr.From) && !t.After(tr.To)
}

func (d *Datasource) annotations(ctx context.Context, q Query, dq backend.DataQuery) (data.Frames, error) {
	var times, ends []*time.Time
	var titles, texts []string
	var tags [][]string
	add := func(t time.Time, end *time.Time, title, text string, tg ...string) {
		tt := t
		times, ends, titles, texts, tags = append(times, &tt), append(ends, end), append(titles, title), append(texts, text), append(tags, tg)
	}
	switch q.QueryType {
	case QTAnnReleases:
		var res struct {
			Releases []Row `json:"releases"`
		}
		if err := d.client.Get(ctx, "projects/"+proj(q)+"/releases", nil, &res); err != nil {
			return nil, err
		}
		for _, r := range res.Releases {
			if t, ok := parseTime(r["first_seen"]); ok && inRange(t, dq.TimeRange) {
				v := str(r["version"])
				add(t, nil, "Release "+v, "First seen: release "+v, "release", v)
			}
		}
	case QTAnnIncidents:
		rows, err := d.incidentRows(ctx, q, dq)
		if err != nil {
			return nil, err
		}
		for _, r := range rows {
			t, ok := parseTime(r["started_at"])
			if !ok {
				continue
			}
			var end *time.Time
			if e, ok := parseTime(r["ended_at"]); ok {
				end = &e
			}
			if !inRange(t, dq.TimeRange) && (end == nil || !inRange(*end, dq.TimeRange)) {
				continue
			}
			add(t, end, fmt.Sprintf("%s: %s", str(r["monitor_name"]), str(r["kind"])), str(r["first_error"]), "incident", str(r["monitor_slug"]))
		}
	case QTAnnIssues:
		v := url.Values{}
		v.Set("since", fmt.Sprint(dq.TimeRange.From.UnixMilli()))
		v.Set("status", "all")
		v.Set("sort", "first_seen")
		v.Set("limit", "100")
		setIf(v, "release", q.Release)
		setIf(v, "environment", q.Environment)
		var res struct {
			Issues []Row `json:"issues"`
		}
		if err := d.client.Get(ctx, "projects/"+proj(q)+"/issues", v, &res); err != nil {
			return nil, err
		}
		for _, r := range res.Issues {
			if t, ok := parseTime(r["first_seen"]); ok && inRange(t, dq.TimeRange) {
				add(t, nil, "New issue "+str(r["short_id"]), str(r["title"]), "issue", str(r["level"]))
			}
			if t, ok := parseTime(r["regressed_at"]); ok && inRange(t, dq.TimeRange) {
				add(t, nil, "Regression "+str(r["short_id"]), str(r["title"]), "regression", str(r["level"]))
			}
		}
	}
	return data.Frames{annotationFrame(dq.RefID, times, ends, titles, texts, tags)}, nil
}
