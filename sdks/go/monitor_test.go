package vaka_test

import (
	"context"
	"errors"
	"strings"
	"testing"

	vaka "github.com/ErikHellman/vaka/sdks/go"
)

func (f *fakeServer) checkInReqs() []checkInReq {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]checkInReq(nil), f.checkIns...)
}

func TestWithMonitorOK(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	grace := 60
	cfg := &vaka.MonitorConfig{Schedule: vaka.MonitorSchedule{Cron: "0 3 * * *", Timezone: "Europe/Stockholm"}, GraceS: &grace}
	if err := c.WithMonitor(context.Background(), "nightly-backup", cfg, func(ctx context.Context) error { return nil }); err != nil {
		t.Fatal(err)
	}
	reqs := f.checkInReqs()
	if len(reqs) != 2 {
		t.Fatalf("check-ins = %d", len(reqs))
	}
	if reqs[0].path != "/ingest/v1/monitors/nightly-backup/check-ins" {
		t.Errorf("path = %s", reqs[0].path)
	}
	first, last := reqs[0].body, reqs[1].body
	if first["status"] != "in_progress" || last["status"] != "ok" {
		t.Errorf("statuses %v %v", first["status"], last["status"])
	}
	if first["check_in_id"] == nil || first["check_in_id"] != last["check_in_id"] {
		t.Errorf("ids %v %v", first["check_in_id"], last["check_in_id"])
	}
	mon, _ := first["monitor"].(map[string]any)
	sched, _ := mon["schedule"].(map[string]any)
	if sched["cron"] != "0 3 * * *" || mon["grace_s"] != float64(60) {
		t.Errorf("monitor = %v", first["monitor"])
	}
	if last["monitor"] != nil || first["release"] != "1.2.3" || first["environment"] != "staging" {
		t.Errorf("bodies %v %v", first, last)
	}
}

func TestWithMonitorError(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	want := errors.New("disk full")
	err := c.WithMonitor(context.Background(), "sync", nil, func(ctx context.Context) error { return want })
	if !errors.Is(err, want) {
		t.Fatalf("err = %v", err)
	}
	reqs := f.checkInReqs()
	if len(reqs) != 2 || reqs[1].body["status"] != "error" || reqs[1].body["message"] != "disk full" {
		t.Fatalf("check-ins = %+v", reqs)
	}
	id := reqs[0].body["check_in_id"]
	errs := f.byType("error")
	if len(errs) == 0 {
		t.Fatal("error not captured")
	}
	tags, _ := errs[0]["tags"].(map[string]any)
	if tags[vaka.CheckInTag] != id || tags["monitor.slug"] != "sync" {
		t.Errorf("tags = %v, want check-in id %v", tags, id)
	}
}

func TestCheckInErrors(t *testing.T) {
	if err := (*vaka.Client)(nil).CheckIn(context.Background(), "x", vaka.CheckIn{}); !errors.Is(err, vaka.ErrNotInitialized) {
		t.Errorf("nil client: %v", err)
	}
	f := newFake(t)
	c := initClient(t, f, nil)
	err := c.CheckIn(context.Background(), "missing", vaka.CheckIn{})
	if err == nil || !strings.Contains(err.Error(), "404") || !strings.Contains(err.Error(), "no monitor") {
		t.Errorf("err = %v", err)
	}
	if err := c.CheckIn(context.Background(), "ok-job", vaka.CheckIn{DurationMs: 42}); err != nil {
		t.Fatal(err)
	}
	reqs := f.checkInReqs()
	if last := reqs[len(reqs)-1].body; last["status"] != "ok" || last["duration_ms"] != float64(42) {
		t.Errorf("body = %v", last)
	}
}
