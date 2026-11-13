package vaka

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"sync"
	"time"
)

const (
	maxQueue  = 1000
	batchSize = 50
)

type flushReq struct{ done chan struct{} }

// transport batches items and posts them to /ingest/v1/batch on a background goroutine.
type transport struct {
	cfg      Config
	items    chan map[string]any
	flushes  chan flushReq
	stop     chan struct{}
	stopped  chan struct{}
	once     sync.Once
	mu       sync.Mutex
	disabled bool // after a 401
}

func newTransport(cfg Config) *transport {
	t := &transport{
		cfg:     cfg,
		items:   make(chan map[string]any, maxQueue),
		flushes: make(chan flushReq),
		stop:    make(chan struct{}),
		stopped: make(chan struct{}),
	}
	go t.loop()
	return t
}

func (t *transport) logf(format string, args ...any) {
	if t.cfg.Debug {
		fmt.Fprintf(os.Stderr, "[vaka] "+format+"\n", args...)
	}
}

func (t *transport) enqueue(item map[string]any) {
	select {
	case <-t.stopped:
		return
	default:
	}
	select {
	case t.items <- item:
	default:
		t.logf("queue full; dropping %v item", item["type"])
	}
}

func (t *transport) loop() {
	defer close(t.stopped)
	var queue []map[string]any
	var backoff time.Duration
	timer := time.NewTimer(t.cfg.FlushInterval)
	defer timer.Stop()
	send := func() {
		queue, backoff = t.send(queue, backoff)
		if !timer.Stop() {
			select {
			case <-timer.C:
			default:
			}
		}
		timer.Reset(t.cfg.FlushInterval + backoff)
	}
	drain := func() {
		for {
			select {
			case it := <-t.items:
				queue = append(queue, it)
			default:
				return
			}
		}
	}
	for {
		select {
		case it := <-t.items:
			queue = append(queue, it)
			if it["type"] == "error" || len(queue) >= batchSize {
				drain()
				send()
			}
		case <-timer.C:
			drain()
			send()
		case r := <-t.flushes:
			drain()
			queue, _ = t.send(queue, 0)
			close(r.done)
		case <-t.stop:
			drain()
			queue, _ = t.send(queue, 0)
			return
		}
	}
}

// send posts the queue in batches and returns what is left (for retry) and the next backoff.
func (t *transport) send(queue []map[string]any, backoff time.Duration) ([]map[string]any, time.Duration) {
	for len(queue) > 0 {
		t.mu.Lock()
		off := t.disabled
		t.mu.Unlock()
		if off {
			return nil, 0
		}
		n := min(len(queue), maxQueue)
		status, err := t.post(queue[:n])
		switch {
		case err != nil || status == 429 || status >= 500:
			if err != nil {
				t.logf("send failed: %v", err)
			} else {
				t.logf("ingest returned %d; retrying later", status)
			}
			backoff = min(max(backoff*2, 5*time.Second), 5*time.Minute)
			if len(queue) > maxQueue {
				queue = queue[len(queue)-maxQueue:]
			}
			return queue, backoff
		case status == 401:
			t.logf("ingest key rejected (401); SDK stops sending until restart")
			t.mu.Lock()
			t.disabled = true
			t.mu.Unlock()
			return nil, 0
		default:
			t.logf("sent %d items (%d)", n, status)
			queue = queue[n:]
		}
	}
	return nil, 0
}

func (t *transport) post(items []map[string]any) (int, error) {
	body, err := json.Marshal(map[string]any{
		"sdk":   map[string]any{"name": sdkName, "version": Version},
		"items": items,
	})
	if err != nil {
		return 0, err
	}
	req, err := http.NewRequest(http.MethodPost, t.cfg.Endpoint+"/ingest/v1/batch", bytes.NewReader(body))
	if err != nil {
		return 0, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Vaka-Key", t.cfg.Key)
	resp, err := t.cfg.HTTPClient.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(io.LimitReader(resp.Body, 64<<10))
	if resp.StatusCode >= 300 || (t.cfg.Debug && bytes.Contains(b, []byte(`"error"`))) {
		t.logf("ingest response %d: %s", resp.StatusCode, bytes.TrimSpace(b))
	}
	return resp.StatusCode, nil
}

func (t *transport) flush(ctx context.Context) error {
	r := flushReq{done: make(chan struct{})}
	select {
	case t.flushes <- r:
	case <-t.stopped:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
	select {
	case <-r.done:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (t *transport) close(ctx context.Context) error {
	t.once.Do(func() { close(t.stop) })
	select {
	case <-t.stopped:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}
