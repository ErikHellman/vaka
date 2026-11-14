package vaka_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"mime"
	"mime/multipart"
	"testing"

	vaka "github.com/ErikHellman/vaka/sdks/go"
)

func (f *fakeServer) feedbackReqs() []feedbackReq {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]feedbackReq(nil), f.feedback...)
}

func TestCaptureFeedbackJSON(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	ctx := vaka.WithScope(context.Background())
	vaka.SetUser(ctx, "user-7")
	vaka.SetTag(ctx, "tenant", "acme")
	vaka.SetFlag(ctx, "new_export", "on")
	eventID := c.CaptureError(ctx, errors.New("export failed"))
	r, err := c.CaptureFeedback(ctx, vaka.Feedback{
		Message:   "  The export button does nothing  ",
		Email:     "ann@example.com",
		EventID:   eventID,
		Issue:     "API-12",
		SessionID: "s-1",
		IP:        "203.0.113.9",
		Tags:      map[string]string{"plan": "pro"},
	})
	if err != nil {
		t.Fatal(err)
	}
	if r.ID != "fb-1" || r.ShortID != "API-F1" {
		t.Errorf("result = %+v", r)
	}
	reqs := f.feedbackReqs()
	if len(reqs) != 1 {
		t.Fatalf("feedback requests = %d", len(reqs))
	}
	req := reqs[0]
	if req.header.Get("X-Forwarded-For") != "203.0.113.9" || req.header.Get("X-Vaka-Key") != "vk_test" {
		t.Errorf("headers = %v", req.header)
	}
	var item map[string]any
	if err := json.Unmarshal(req.body, &item); err != nil {
		t.Fatal(err)
	}
	want := map[string]any{
		"message": "The export button does nothing", "email": "ann@example.com", "event_id": eventID,
		"issue": "API-12", "session_id": "s-1", "user_id": "user-7", "source": "api", "platform": "go",
		"release": "1.2.3", "environment": "staging",
	}
	for k, v := range want {
		if item[k] != v {
			t.Errorf("%s = %v, want %v", k, item[k], v)
		}
	}
	tags, _ := item["tags"].(map[string]any)
	if tags["tenant"] != "acme" || tags["plan"] != "pro" {
		t.Errorf("tags = %v", item["tags"])
	}
	if flags, _ := item["flags"].(map[string]any); flags["new_export"] != "on" {
		t.Errorf("flags = %v", item["flags"])
	}
}

func TestCaptureFeedbackMultipart(t *testing.T) {
	f := newFake(t)
	c := initClient(t, f, nil)
	png := []byte("\x89PNG\r\n\x1a\n0000")
	if _, err := c.CaptureFeedback(context.Background(), vaka.Feedback{
		Message:     "with a screenshot",
		Attachments: []vaka.Attachment{{Filename: "shot.png", Data: png}},
	}); err != nil {
		t.Fatal(err)
	}
	req := f.feedbackReqs()[0]
	mt, params, err := mime.ParseMediaType(req.header.Get("Content-Type"))
	if err != nil || mt != "multipart/form-data" {
		t.Fatalf("content type %q: %v", req.header.Get("Content-Type"), err)
	}
	mr := multipart.NewReader(bytes.NewReader(req.body), params["boundary"])
	p, _ := mr.NextPart()
	b, _ := io.ReadAll(p)
	if p.FormName() != "feedback" || !bytes.Contains(b, []byte(`"message":"with a screenshot"`)) {
		t.Errorf("first part %s: %s", p.FormName(), b)
	}
	p, _ = mr.NextPart()
	b, _ = io.ReadAll(p)
	if p.FileName() != "shot.png" || p.Header.Get("Content-Type") != "image/png" || !bytes.Equal(b, png) {
		t.Errorf("attachment %s %s %q", p.FileName(), p.Header.Get("Content-Type"), b)
	}
}

func TestCaptureFeedbackErrors(t *testing.T) {
	if _, err := (*vaka.Client)(nil).CaptureFeedback(context.Background(), vaka.Feedback{Message: "x"}); !errors.Is(err, vaka.ErrNotInitialized) {
		t.Errorf("nil client: %v", err)
	}
	f := newFake(t)
	c := initClient(t, f, nil)
	if _, err := c.CaptureFeedback(context.Background(), vaka.Feedback{Message: "  "}); err == nil {
		t.Error("empty message accepted")
	}
	four := make([]vaka.Attachment, 4)
	if _, err := c.CaptureFeedback(context.Background(), vaka.Feedback{Message: "x", Attachments: four}); err == nil {
		t.Error("4 attachments accepted")
	}
	f.mu.Lock()
	f.fbStatus = 429
	f.mu.Unlock()
	_, err := c.CaptureFeedback(context.Background(), vaka.Feedback{Message: "again"})
	var fe *vaka.FeedbackError
	if !errors.As(err, &fe) || fe.StatusCode != 429 || fe.Message != "too much feedback" {
		t.Errorf("err = %v", err)
	}
}
