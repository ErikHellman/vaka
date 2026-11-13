package vaka

import (
	"context"
	"fmt"
	"maps"
	"sync"
	"time"
)

// MaxBreadcrumbs is how many breadcrumbs a scope keeps (the newest win).
const MaxBreadcrumbs = 100

// Level is the severity of an error, message or breadcrumb.
type Level string

const (
	LevelFatal   Level = "fatal"
	LevelError   Level = "error"
	LevelWarning Level = "warning"
	LevelInfo    Level = "info"
	LevelDebug   Level = "debug"
)

// Breadcrumb is something that happened before an error. Categories with special meaning:
// navigation, http (Data: method, url, status_code, duration_ms), ui.click, console, lifecycle.
type Breadcrumb struct {
	Timestamp time.Time
	Category  string
	Message   string
	Level     Level
	Data      map[string]any
}

func (b Breadcrumb) item() map[string]any {
	m := map[string]any{
		"timestamp": b.Timestamp.UTC().Format(time.RFC3339Nano),
		"category":  b.Category,
		"message":   b.Message,
		"level":     string(b.Level),
	}
	if len(b.Data) > 0 {
		m["data"] = b.Data
	}
	return m
}

// Scope holds the user, tags, breadcrumbs and flag values attached to what is sent.
// The client has a global scope; WithScope gives a request or job its own copy.
type Scope struct {
	mu          sync.Mutex
	userID      string
	tags        map[string]string
	contexts    map[string]map[string]any
	flags       map[string]string
	breadcrumbs []Breadcrumb
}

// NewScope returns an empty scope.
func NewScope() *Scope {
	return &Scope{tags: map[string]string{}, contexts: map[string]map[string]any{}, flags: map[string]string{}}
}

// Clone copies the scope; changes to the copy do not affect the original.
func (s *Scope) Clone() *Scope {
	s.mu.Lock()
	defer s.mu.Unlock()
	n := NewScope()
	n.userID = s.userID
	maps.Copy(n.tags, s.tags)
	maps.Copy(n.flags, s.flags)
	for k, v := range s.contexts {
		n.contexts[k] = maps.Clone(v)
	}
	n.breadcrumbs = append([]Breadcrumb(nil), s.breadcrumbs...)
	return n
}

// SetUser sets the identified user id (empty clears it). Opt-in: only identify users when
// your privacy policy allows it.
func (s *Scope) SetUser(id string) {
	s.mu.Lock()
	s.userID = id
	s.mu.Unlock()
}

// SetTag sets a tag sent with every error.
func (s *Scope) SetTag(key, value string) {
	s.mu.Lock()
	s.tags[key] = value
	s.mu.Unlock()
}

// SetContext sets a named context object (for example "job" with id and attempt) sent with errors.
func (s *Scope) SetContext(name string, values map[string]any) {
	s.mu.Lock()
	if values == nil {
		delete(s.contexts, name)
	} else {
		s.contexts[name] = maps.Clone(values)
	}
	s.mu.Unlock()
}

// SetFlag records a feature flag value this code path used. Values are sent as `flags` on
// errors, events and sessions, so Vaka can split crash and conversion numbers by variant.
// Record flags that are off too ("false").
func (s *Scope) SetFlag(key string, value any) {
	s.mu.Lock()
	s.flags[key] = fmt.Sprint(value)
	s.mu.Unlock()
}

// AddBreadcrumb records a breadcrumb, keeping the newest MaxBreadcrumbs.
func (s *Scope) AddBreadcrumb(b Breadcrumb) {
	if b.Timestamp.IsZero() {
		b.Timestamp = time.Now()
	}
	if b.Level == "" {
		b.Level = LevelInfo
	}
	s.mu.Lock()
	s.breadcrumbs = append(s.breadcrumbs, b)
	if n := len(s.breadcrumbs); n > MaxBreadcrumbs {
		s.breadcrumbs = append([]Breadcrumb(nil), s.breadcrumbs[n-MaxBreadcrumbs:]...)
	}
	s.mu.Unlock()
}

// ClearBreadcrumbs removes all breadcrumbs.
func (s *Scope) ClearBreadcrumbs() {
	s.mu.Lock()
	s.breadcrumbs = nil
	s.mu.Unlock()
}

// apply writes the scope's fields into an item. Item fields already set win.
func (s *Scope) apply(item map[string]any, withBreadcrumbs bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.userID != "" {
		if _, ok := item["user_id"]; !ok {
			item["user_id"] = s.userID
		}
		if item["type"] == "error" {
			item["user"] = map[string]any{"id": s.userID}
		}
	}
	if len(s.flags) > 0 {
		f, _ := item["flags"].(map[string]string)
		if f == nil {
			f = map[string]string{}
		}
		for k, v := range s.flags {
			if _, ok := f[k]; !ok {
				f[k] = v
			}
		}
		item["flags"] = f
	}
	if item["type"] != "error" {
		return
	}
	if len(s.tags) > 0 {
		t, _ := item["tags"].(map[string]string)
		if t == nil {
			t = map[string]string{}
		}
		for k, v := range s.tags {
			if _, ok := t[k]; !ok {
				t[k] = v
			}
		}
		item["tags"] = t
	}
	if len(s.contexts) > 0 {
		c, _ := item["contexts"].(map[string]any)
		if c == nil {
			c = map[string]any{}
		}
		for k, v := range s.contexts {
			if _, ok := c[k]; !ok {
				c[k] = maps.Clone(v)
			}
		}
		item["contexts"] = c
	}
	if withBreadcrumbs && len(s.breadcrumbs) > 0 {
		bs := make([]map[string]any, len(s.breadcrumbs))
		for i, b := range s.breadcrumbs {
			bs[i] = b.item()
		}
		item["breadcrumbs"] = bs
	}
}

type scopeKey struct{}

// WithScope returns a context with its own scope, copied from the scope already in ctx (or
// the client's global scope). Use one per request or job, so users, tags, flags and
// breadcrumbs from concurrent requests don't mix. Middleware does this for HTTP handlers.
func WithScope(ctx context.Context) context.Context {
	parent := ScopeFromContext(ctx)
	return context.WithValue(ctx, scopeKey{}, parent.Clone())
}

// ScopeFromContext returns the scope in ctx, or the current client's global scope.
func ScopeFromContext(ctx context.Context) *Scope {
	if ctx != nil {
		if s, ok := ctx.Value(scopeKey{}).(*Scope); ok {
			return s
		}
	}
	return CurrentClient().Scope()
}

// AddBreadcrumb records a breadcrumb on the scope in ctx (or the global scope).
func AddBreadcrumb(ctx context.Context, category, message string, level Level) {
	ScopeFromContext(ctx).AddBreadcrumb(Breadcrumb{Category: category, Message: message, Level: level})
}

// SetUser sets the user id on the scope in ctx (or the global scope). Opt-in.
func SetUser(ctx context.Context, id string) { ScopeFromContext(ctx).SetUser(id) }

// SetTag sets a tag on the scope in ctx (or the global scope).
func SetTag(ctx context.Context, key, value string) { ScopeFromContext(ctx).SetTag(key, value) }

// SetFlag records a flag value on the scope in ctx (or the global scope).
func SetFlag(ctx context.Context, key string, value any) {
	ScopeFromContext(ctx).SetFlag(key, value)
}
