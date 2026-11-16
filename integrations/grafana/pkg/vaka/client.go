// Package vaka is a small client for the Vaka REST API (https://vaka.hellsoft.se/api.html).
package vaka

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// Client calls one Vaka server with one API token.
type Client struct {
	BaseURL string
	Token   string
	HTTP    *http.Client
	// UserAgent is sent with every request.
	UserAgent string
}

// APIError is a non-2xx response; Vaka returns {"error": "..."}.
type APIError struct {
	Status  int
	Message string
}

func (e *APIError) Error() string {
	switch e.Status {
	case http.StatusUnauthorized:
		return "Vaka rejected the API token (401): " + e.Message
	case http.StatusForbidden:
		return "the API token has no access to this (403): " + e.Message
	case http.StatusNotFound:
		return "not found (404): " + e.Message
	}
	return fmt.Sprintf("Vaka returned %d: %s", e.Status, e.Message)
}

// IsNotFound reports whether err is a 404 from Vaka (e.g. an endpoint an older server lacks).
func IsNotFound(err error) bool {
	var e *APIError
	return errors.As(err, &e) && e.Status == http.StatusNotFound
}

func (c *Client) newRequest(ctx context.Context, path string, q url.Values) (*http.Request, error) {
	u := c.BaseURL + "/api/v1/" + strings.TrimLeft(path, "/")
	if len(q) > 0 {
		u += "?" + q.Encode()
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return nil, err
	}
	if c.Token != "" {
		req.Header.Set("Authorization", "Bearer "+c.Token)
	}
	if c.UserAgent != "" {
		req.Header.Set("User-Agent", c.UserAgent)
	}
	return req, nil
}

// Get fetches path (relative to /api/v1) and decodes the JSON body into out.
func (c *Client) Get(ctx context.Context, path string, q url.Values, out any) error {
	req, err := c.newRequest(ctx, path, q)
	if err != nil {
		return err
	}
	req.Header.Set("Accept", "application/json")
	res, err := c.HTTP.Do(req)
	if err != nil {
		return fmt.Errorf("calling Vaka: %w", err)
	}
	defer func() { _ = res.Body.Close() }()
	body, err := io.ReadAll(io.LimitReader(res.Body, 64<<20))
	if err != nil {
		return fmt.Errorf("reading Vaka response: %w", err)
	}
	if res.StatusCode < 200 || res.StatusCode > 299 {
		var e struct {
			Error string `json:"error"`
		}
		msg := strings.TrimSpace(string(body))
		if json.Unmarshal(body, &e) == nil && e.Error != "" {
			msg = e.Error
		}
		if len(msg) > 300 {
			msg = msg[:300]
		}
		return &APIError{Status: res.StatusCode, Message: msg}
	}
	if out == nil {
		return nil
	}
	if err := json.Unmarshal(body, out); err != nil {
		return fmt.Errorf("decoding Vaka response: %w", err)
	}
	return nil
}

// Event is one Server-Sent Event from the live stream.
type Event struct {
	Name string
	ID   string
	Data json.RawMessage
}

// Stream opens GET path as Server-Sent Events and calls fn for each event until ctx is done,
// the server closes the stream, or fn returns an error. The HTTP client must not have a
// whole-request timeout.
func (c *Client) Stream(ctx context.Context, path string, q url.Values, lastID string, fn func(Event) error) error {
	req, err := c.newRequest(ctx, path, q)
	if err != nil {
		return err
	}
	req.Header.Set("Accept", "text/event-stream")
	if lastID != "" {
		req.Header.Set("Last-Event-ID", lastID)
	}
	res, err := c.HTTP.Do(req)
	if err != nil {
		return fmt.Errorf("opening Vaka stream: %w", err)
	}
	defer func() { _ = res.Body.Close() }()
	if res.StatusCode != http.StatusOK {
		body, _ := io.ReadAll(io.LimitReader(res.Body, 4096))
		var e struct {
			Error string `json:"error"`
		}
		msg := strings.TrimSpace(string(body))
		if json.Unmarshal(body, &e) == nil && e.Error != "" {
			msg = e.Error
		}
		return &APIError{Status: res.StatusCode, Message: msg}
	}
	return ParseSSE(res.Body, fn)
}

// ParseSSE reads an event stream and calls fn per event.
func ParseSSE(r io.Reader, fn func(Event) error) error {
	sc := bufio.NewScanner(r)
	sc.Buffer(make([]byte, 64*1024), 4<<20)
	var ev Event
	var data []string
	flush := func() error {
		if len(data) == 0 && ev.Name == "" {
			return nil
		}
		ev.Data = json.RawMessage(strings.Join(data, "\n"))
		if ev.Name == "" {
			ev.Name = "message"
		}
		err := fn(ev)
		ev, data = Event{}, nil
		return err
	}
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			if err := flush(); err != nil {
				return err
			}
			continue
		}
		if strings.HasPrefix(line, ":") {
			continue // comment / keep-alive
		}
		field, value, _ := strings.Cut(line, ":")
		value = strings.TrimPrefix(value, " ")
		switch field {
		case "event":
			ev.Name = value
		case "data":
			data = append(data, value)
		case "id":
			ev.ID = value
		}
	}
	if err := sc.Err(); err != nil {
		return err
	}
	return flush()
}

// UnixMilli formats t for Vaka time parameters (unix milliseconds).
func UnixMilli(t time.Time) string {
	return fmt.Sprintf("%d", t.UnixMilli())
}
