package vaka

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"time"
)

// CheckInTag is the error tag that links errors captured during a monitored run to the run.
const CheckInTag = "monitor.check_in_id"

// Check-in statuses for heartbeat monitors.
const (
	CheckInInProgress = "in_progress"
	CheckInOK         = "ok"
	CheckInError      = "error"
)

// MonitorSchedule is when a heartbeat monitor expects its job: set Every ("15m") or Cron
// ("0 3 * * *") with an optional IANA Timezone.
type MonitorSchedule struct {
	Every    string `json:"every,omitempty"`
	Cron     string `json:"cron,omitempty"`
	Timezone string `json:"timezone,omitempty"`
}

// MonitorConfig creates a monitor on its first check-in, so jobs can define monitors in code.
type MonitorConfig struct {
	Schedule MonitorSchedule `json:"schedule"`
	// GraceS is how many seconds a check-in may be late (server default 120).
	GraceS *int `json:"grace_s,omitempty"`
	// MaxRuntimeS is how many seconds a run may take (server default 1800, 0 = no limit).
	MaxRuntimeS *int   `json:"max_runtime_s,omitempty"`
	Name        string `json:"name,omitempty"`
}

// CheckIn is one heartbeat report for a monitor.
type CheckIn struct {
	// Status is CheckInInProgress, CheckInOK (default) or CheckInError.
	Status string
	// ID pairs an in_progress check-in with its ok or error.
	ID         string
	DurationMs int64
	// Message is the error text for error check-ins.
	Message string
	Monitor *MonitorConfig
}

// CheckIn reports a heartbeat check-in for the monitor slug. It returns an error when the
// client is not initialized or the server refuses the check-in; jobs usually just log it.
func (c *Client) CheckIn(ctx context.Context, slug string, ci CheckIn) error {
	if !c.enabled() {
		return ErrNotInitialized
	}
	if ctx == nil {
		ctx = context.Background()
	}
	status := ci.Status
	if status == "" {
		status = CheckInOK
	}
	body := map[string]any{"status": status, "environment": c.cfg.Environment}
	if c.cfg.Release != "" {
		body["release"] = c.cfg.Release
	}
	if ci.ID != "" {
		body["check_in_id"] = ci.ID
	}
	if ci.DurationMs > 0 {
		body["duration_ms"] = ci.DurationMs
	}
	if ci.Message != "" {
		m := []rune(ci.Message)
		if len(m) > 500 {
			m = m[:500]
		}
		body["message"] = string(m)
	}
	if ci.Monitor != nil {
		body["monitor"] = ci.Monitor
	}
	data, err := json.Marshal(body)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	hr, err := http.NewRequestWithContext(ctx, http.MethodPost, c.cfg.Endpoint+"/ingest/v1/monitors/"+url.PathEscape(slug)+"/check-ins", bytes.NewReader(data))
	if err != nil {
		return err
	}
	hr.Header.Set("Content-Type", "application/json")
	hr.Header.Set("X-Vaka-Key", c.cfg.Key)
	hr.Header.Set("User-Agent", sdkName+"/"+Version)
	resp, err := c.cfg.HTTPClient.Do(hr)
	if err != nil {
		return fmt.Errorf("vaka: check-in for %s: %w", slug, err)
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		var e struct {
			Error string `json:"error"`
		}
		_ = json.Unmarshal(b, &e)
		return fmt.Errorf("vaka: check-in for %s: HTTP %d: %s", slug, resp.StatusCode, e.Error)
	}
	return nil
}

// WithMonitor runs fn as a monitored run of the monitor slug: it reports in_progress, then ok
// with the duration, or error when fn returns an error or panics (the error is captured with
// the run's id as a tag; a panic is reported and re-panicked). Pass a config to create the
// monitor on first use, or nil. Check-in failures are logged to stderr, never returned.
//
//	err := vaka.WithMonitor(ctx, "nightly-backup", &vaka.MonitorConfig{Schedule: vaka.MonitorSchedule{Cron: "0 3 * * *"}}, backup)
func (c *Client) WithMonitor(ctx context.Context, slug string, cfg *MonitorConfig, fn func(ctx context.Context) error) (err error) {
	if ctx == nil {
		ctx = context.Background()
	}
	id := newID()
	report := func(ci CheckIn) {
		if e := c.CheckIn(ctx, slug, ci); e != nil && e != ErrNotInitialized {
			fmt.Fprintln(os.Stderr, e)
		}
	}
	report(CheckIn{Status: CheckInInProgress, ID: id, Monitor: cfg})
	runCtx := WithScope(ctx)
	scope := ScopeFromContext(runCtx)
	scope.SetTag(CheckInTag, id)
	scope.SetTag("monitor.slug", slug)
	started := time.Now()
	defer func() {
		ms := time.Since(started).Milliseconds()
		if v := recover(); v != nil {
			report(CheckIn{Status: CheckInError, ID: id, DurationMs: ms, Message: fmt.Sprint(v)})
			c.crash(runCtx, v)
			panic(v)
		}
		if err != nil {
			if c.enabled() {
				c.CaptureError(runCtx, err)
				fctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				_ = c.Flush(fctx)
				cancel()
			}
			report(CheckIn{Status: CheckInError, ID: id, DurationMs: ms, Message: err.Error()})
			return
		}
		report(CheckIn{Status: CheckInOK, ID: id, DurationMs: ms})
	}()
	return fn(runCtx)
}

// SendCheckIn reports a check-in with the current client. See Client.CheckIn.
func SendCheckIn(ctx context.Context, slug string, ci CheckIn) error {
	return CurrentClient().CheckIn(ctx, slug, ci)
}

// WithMonitor runs fn as a monitored run with the current client. See Client.WithMonitor.
func WithMonitor(ctx context.Context, slug string, cfg *MonitorConfig, fn func(ctx context.Context) error) error {
	return CurrentClient().WithMonitor(ctx, slug, cfg, fn)
}
