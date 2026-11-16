package plugin

import (
	"context"
	"encoding/json"
	"fmt"
	"net/url"
	"regexp"
	"strings"
	"time"

	"github.com/ErikHellman/vaka/integrations/grafana/pkg/vaka"
	"github.com/grafana/grafana-plugin-sdk-go/backend"
	"github.com/grafana/grafana-plugin-sdk-go/backend/log"
	"github.com/grafana/grafana-plugin-sdk-go/data"
)

// Live channels are ds/<uid>/<stream>/<project>[/<environment>], stream one of errors, events,
// spans. Grafana runs one RunStream per channel and shares it between all viewers.

var segment = regexp.MustCompile(`^[A-Za-z0-9_.\-]+$`)

type channel struct {
	Stream      string
	Project     string
	Environment string
}

func parseChannel(path string) (channel, error) {
	parts := strings.Split(strings.Trim(path, "/"), "/")
	if len(parts) < 2 || len(parts) > 3 {
		return channel{}, fmt.Errorf("bad channel path %q", path)
	}
	for _, p := range parts {
		if !segment.MatchString(p) {
			return channel{}, fmt.Errorf("bad channel path %q", path)
		}
	}
	c := channel{Stream: parts[0], Project: parts[1]}
	if len(parts) == 3 {
		c.Environment = parts[2]
	}
	switch c.Stream {
	case "errors", "events", "spans":
	default:
		return channel{}, fmt.Errorf("unknown stream %q: errors, events or spans", c.Stream)
	}
	return c, nil
}

// SubscribeStream allows a subscription when the token can read the project.
func (d *Datasource) SubscribeStream(ctx context.Context, req *backend.SubscribeStreamRequest) (*backend.SubscribeStreamResponse, error) {
	c, err := parseChannel(req.Path)
	if err != nil {
		return &backend.SubscribeStreamResponse{Status: backend.SubscribeStreamStatusNotFound}, nil
	}
	if err := d.client.Get(ctx, "projects/"+url.PathEscape(c.Project), nil, nil); err != nil {
		log.DefaultLogger.Warn("live subscription refused", "path", req.Path, "error", err)
		return &backend.SubscribeStreamResponse{Status: backend.SubscribeStreamStatusPermissionDenied}, nil
	}
	return &backend.SubscribeStreamResponse{Status: backend.SubscribeStreamStatusOK}, nil
}

// PublishStream: viewers cannot publish into Vaka channels.
func (d *Datasource) PublishStream(context.Context, *backend.PublishStreamRequest) (*backend.PublishStreamResponse, error) {
	return &backend.PublishStreamResponse{Status: backend.PublishStreamStatusPermissionDenied}, nil
}

// RunStream relays the Vaka SSE stream into the channel, reconnecting with backoff until
// the last viewer leaves (ctx is cancelled).
func (d *Datasource) RunStream(ctx context.Context, req *backend.RunStreamRequest, sender *backend.StreamSender) error {
	c, err := parseChannel(req.Path)
	if err != nil {
		return err
	}
	q := url.Values{}
	q.Set("types", strings.TrimSuffix(c.Stream, "s"))
	if c.Environment != "" {
		q.Set("environment", c.Environment)
	}
	path := "projects/" + url.PathEscape(c.Project) + "/stream"
	backoff := time.Second
	lastID := ""
	for {
		started := time.Now()
		err := d.stream.Stream(ctx, path, q, lastID, func(ev vaka.Event) error {
			if ev.ID != "" {
				lastID = ev.ID
			}
			frame, ok := liveEventFrame(c.Stream, ev)
			if !ok {
				return nil
			}
			return sender.SendFrame(frame, data.IncludeAll)
		})
		if ctx.Err() != nil {
			return nil
		}
		if err != nil {
			log.DefaultLogger.Warn("vaka stream disconnected", "path", req.Path, "error", err)
			if vaka.IsNotFound(err) {
				// No stream endpoint (older Vaka) or no such project: do not hammer the server.
				backoff = 5 * time.Minute
			}
		}
		if time.Since(started) > time.Minute {
			backoff = time.Second
		}
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(backoff):
		}
		if backoff < time.Minute {
			backoff *= 2
		}
	}
}

// liveEventFrame turns one stream event into a one-row frame.
func liveEventFrame(stream string, ev vaka.Event) (*data.Frame, bool) {
	want := map[string]string{"errors": "error", "events": "event", "spans": "span"}[stream]
	if ev.Name != want {
		return nil, false
	}
	var m Row
	if err := json.Unmarshal(ev.Data, &m); err != nil {
		return nil, false
	}
	switch stream {
	case "errors":
		title := str(m["exception_type"])
		if v := str(m["exception_value"]); v != "" {
			title += ": " + v
		}
		if title == "" {
			title = str(m["message"])
		}
		m["title"] = title
		return liveRow(m, "timestamp", []string{"title", "level", "release", "environment", "culprit", "handled", "url", "user_id", "issue_id", "event_id", "trace_id"}), true
	case "events":
		return liveRow(m, "timestamp", []string{"type", "name", "path", "country", "browser", "os_name", "release", "environment", "user_id", "session_id"}), true
	default:
		return liveRow(m, "start", []string{"transaction", "op", "name", "duration_ms", "status", "http_status", "release", "environment", "service", "trace_id"}), true
	}
}

func liveRow(m Row, timeKey string, cols []string) *data.Frame {
	t, ok := parseTime(m[timeKey])
	if !ok {
		t = time.Now().UTC()
	}
	f := data.NewFrame("live", data.NewField("time", nil, []time.Time{t}))
	for _, c := range cols {
		switch v := m[c].(type) {
		case float64:
			fl := data.NewField(c, nil, []float64{v})
			fl.Config = metricConfig(c)
			f.Fields = append(f.Fields, fl)
		case bool:
			f.Fields = append(f.Fields, data.NewField(c, nil, []bool{v}))
		default:
			if c == "duration_ms" || c == "http_status" {
				f.Fields = append(f.Fields, data.NewField(c, nil, []float64{0}))
				continue
			}
			f.Fields = append(f.Fields, data.NewField(c, nil, []string{str(v)}))
		}
	}
	return f
}
