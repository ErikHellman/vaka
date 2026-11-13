package vaka

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
)

// FlagRequest identifies who flags are evaluated for.
type FlagRequest struct {
	// DistinctID is a stable id for the person or device (required unless UserID is set).
	DistinctID string `json:"distinct_id,omitempty"`
	// UserID, when set, is used for bucketing and user_ids targeting rules.
	UserID string `json:"user_id,omitempty"`
	// Environment defaults to the client's environment.
	Environment string `json:"environment,omitempty"`
	// Properties are matched by targeting rules, e.g. {"plan": "pro"}.
	Properties map[string]any `json:"properties,omitempty"`
}

// Flags are evaluated flag values: true/false, or the variant key for flags with variants.
type Flags map[string]any

// Enabled reports whether a flag is on (true, or any variant). Unknown flags are off.
func (f Flags) Enabled(key string) bool {
	switch v := f[key].(type) {
	case bool:
		return v
	case string:
		return v != "" && v != "false"
	}
	return false
}

// Variant returns the variant key, "true"/"false" for flags without variants, or
// fallback for unknown flags.
func (f Flags) Variant(key, fallback string) string {
	v, ok := f[key]
	if !ok {
		return fallback
	}
	return fmt.Sprint(v)
}

// EvaluateFlags asks the server for every flag's value for one person
// (POST /ingest/v1/flags). It does not record exposure; call Scope.SetFlag (or use
// WithFlags) for the values the code actually used.
func (c *Client) EvaluateFlags(ctx context.Context, req FlagRequest) (Flags, error) {
	if !c.enabled() {
		return Flags{}, nil
	}
	if req.DistinctID == "" && req.UserID == "" {
		return nil, fmt.Errorf("vaka: FlagRequest needs DistinctID or UserID")
	}
	if req.Environment == "" {
		req.Environment = c.cfg.Environment
	}
	body, err := json.Marshal(req)
	if err != nil {
		return nil, err
	}
	if ctx == nil {
		ctx = context.Background()
	}
	hr, err := http.NewRequestWithContext(ctx, http.MethodPost, c.cfg.Endpoint+"/ingest/v1/flags", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	hr.Header.Set("Content-Type", "application/json")
	hr.Header.Set("X-Vaka-Key", c.cfg.Key)
	resp, err := c.cfg.HTTPClient.Do(hr)
	if err != nil {
		return nil, fmt.Errorf("vaka: evaluate flags: %w", err)
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("vaka: evaluate flags: HTTP %d: %s", resp.StatusCode, bytes.TrimSpace(b))
	}
	var out struct {
		Flags Flags `json:"flags"`
	}
	if err := json.Unmarshal(b, &out); err != nil {
		return nil, fmt.Errorf("vaka: evaluate flags: %w", err)
	}
	if out.Flags == nil {
		out.Flags = Flags{}
	}
	return out.Flags, nil
}

// EvaluateFlags evaluates flags with the current client.
func EvaluateFlags(ctx context.Context, req FlagRequest) (Flags, error) {
	return CurrentClient().EvaluateFlags(ctx, req)
}

// WithFlags returns a context with its own scope (see WithScope) that records every value
// in flags as used, so errors and events sent with the context are split by variant.
func WithFlags(ctx context.Context, flags Flags) context.Context {
	ctx = WithScope(ctx)
	s := ScopeFromContext(ctx)
	for k, v := range flags {
		s.SetFlag(k, v)
	}
	return ctx
}
