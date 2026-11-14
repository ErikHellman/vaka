package vaka

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"mime/multipart"
	"net/http"
	"net/textproto"
	"path"
	"strings"
)

// MaxFeedbackAttachments is how many images one feedback item can carry.
const MaxFeedbackAttachments = 3

const maxFeedbackChars = 5000

// Feedback is what a user wrote, for example in a support form or in-app chat.
type Feedback struct {
	// Message is required. Longer than 5000 characters is cut.
	Message string
	// Email and Name are contact details, only when the user gave them.
	Email string
	Name  string
	// EventID links the feedback to an error event (the id CaptureError returned).
	EventID string
	// Issue links the feedback to an issue: its id or short id like "API-12".
	Issue string
	// UserID defaults to the scope's user.
	UserID string
	// SessionID is the end user's session, when your app has one.
	SessionID string
	// URL is the page or screen the feedback is about.
	URL string
	// IP is the end user's address. Feedback is rate limited per address, so without it all
	// feedback from this server shares one limit.
	IP   string
	Tags map[string]string
	// Attachments: up to 3 PNG, JPEG or WebP images, 5 MB each.
	Attachments []Attachment
}

// Attachment is a file sent with feedback.
type Attachment struct {
	Filename string
	// ContentType defaults to one guessed from Filename.
	ContentType string
	Data        []byte
}

// FeedbackResult identifies the stored feedback.
type FeedbackResult struct {
	ID string `json:"id"`
	// ShortID looks like "API-F12".
	ShortID string `json:"short_id"`
}

// FeedbackError is returned when the server refuses feedback. StatusCode 429 means a
// rate limit was hit.
type FeedbackError struct {
	StatusCode int
	Message    string
}

func (e *FeedbackError) Error() string {
	if e.Message == "" {
		return fmt.Sprintf("vaka: feedback: HTTP %d", e.StatusCode)
	}
	return fmt.Sprintf("vaka: feedback: HTTP %d: %s", e.StatusCode, e.Message)
}

// ErrNotInitialized is returned by CaptureFeedback when no client is running.
var ErrNotInitialized = errors.New("vaka: not initialized")

func contentTypeOf(filename string) string {
	switch strings.ToLower(path.Ext(filename)) {
	case ".png":
		return "image/png"
	case ".webp":
		return "image/webp"
	}
	return "image/jpeg"
}

func (s *Scope) feedbackFields(item map[string]any) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := item["user_id"]; !ok && s.userID != "" {
		item["user_id"] = s.userID
	}
	tags := item["tags"].(map[string]string)
	for k, v := range s.tags {
		if _, ok := tags[k]; !ok {
			tags[k] = v
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
}

// CaptureFeedback forwards feedback a user wrote to Vaka (POST /ingest/v1/feedback). It is
// sent right away, not batched, with the release, environment and the user, tags and flags
// of the scope in ctx. Errors are a *FeedbackError when the server refused it.
func (c *Client) CaptureFeedback(ctx context.Context, f Feedback) (FeedbackResult, error) {
	if !c.enabled() {
		return FeedbackResult{}, ErrNotInitialized
	}
	if ctx == nil {
		ctx = context.Background()
	}
	msg := strings.TrimSpace(f.Message)
	if msg == "" {
		return FeedbackResult{}, errors.New("vaka: feedback needs a message")
	}
	if r := []rune(msg); len(r) > maxFeedbackChars {
		msg = string(r[:maxFeedbackChars])
	}
	if len(f.Attachments) > MaxFeedbackAttachments {
		return FeedbackResult{}, fmt.Errorf("vaka: at most %d attachments per feedback", MaxFeedbackAttachments)
	}
	tags := map[string]string{}
	maps.Copy(tags, f.Tags)
	item := map[string]any{
		"id":          newID(),
		"timestamp":   nowMs(),
		"platform":    "go",
		"source":      "api",
		"message":     msg,
		"release":     c.cfg.Release,
		"environment": c.cfg.Environment,
		"tags":        tags,
	}
	for k, v := range map[string]string{
		"email": strings.TrimSpace(f.Email), "name": strings.TrimSpace(f.Name), "event_id": f.EventID,
		"issue": f.Issue, "user_id": f.UserID, "session_id": f.SessionID, "url": f.URL,
	} {
		if v != "" {
			item[k] = v
		}
	}
	ScopeFromContext(ctx).feedbackFields(item)
	if c.scope != ScopeFromContext(ctx) {
		c.scope.feedbackFields(item)
	}
	payload, err := json.Marshal(item)
	if err != nil {
		return FeedbackResult{}, err
	}
	var body bytes.Buffer
	contentType := "application/json"
	if len(f.Attachments) == 0 {
		body.Write(payload)
	} else {
		mw := multipart.NewWriter(&body)
		h := textproto.MIMEHeader{}
		h.Set("Content-Disposition", `form-data; name="feedback"`)
		h.Set("Content-Type", "application/json")
		pw, _ := mw.CreatePart(h)
		_, _ = pw.Write(payload)
		for _, a := range f.Attachments {
			ct := a.ContentType
			if ct == "" {
				ct = contentTypeOf(a.Filename)
			}
			h := textproto.MIMEHeader{}
			name := strings.NewReplacer(`"`, "", "\r", "", "\n", "").Replace(a.Filename)
			h.Set("Content-Disposition", fmt.Sprintf(`form-data; name="attachment"; filename="%s"`, name))
			h.Set("Content-Type", ct)
			pw, _ := mw.CreatePart(h)
			_, _ = pw.Write(a.Data)
		}
		_ = mw.Close()
		contentType = mw.FormDataContentType()
	}
	hr, err := http.NewRequestWithContext(ctx, http.MethodPost, c.cfg.Endpoint+"/ingest/v1/feedback", &body)
	if err != nil {
		return FeedbackResult{}, err
	}
	hr.Header.Set("Content-Type", contentType)
	hr.Header.Set("X-Vaka-Key", c.cfg.Key)
	hr.Header.Set("User-Agent", sdkName+"/"+Version)
	if f.IP != "" {
		hr.Header.Set("X-Forwarded-For", f.IP)
	}
	resp, err := c.cfg.HTTPClient.Do(hr)
	if err != nil {
		return FeedbackResult{}, fmt.Errorf("vaka: feedback: %w", err)
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		var e struct {
			Error string `json:"error"`
		}
		_ = json.Unmarshal(b, &e)
		return FeedbackResult{}, &FeedbackError{StatusCode: resp.StatusCode, Message: e.Error}
	}
	var out FeedbackResult
	_ = json.Unmarshal(b, &out)
	if out.ID == "" {
		out.ID = item["id"].(string)
	}
	ScopeFromContext(ctx).AddBreadcrumb(Breadcrumb{Category: "feedback", Message: "user feedback sent"})
	return out, nil
}

// CaptureFeedback sends feedback with the current client.
func CaptureFeedback(ctx context.Context, f Feedback) (FeedbackResult, error) {
	return CurrentClient().CaptureFeedback(ctx, f)
}
