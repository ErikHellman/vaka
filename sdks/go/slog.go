package vaka

import (
	"context"
	"errors"
	"log/slog"

	"go.opentelemetry.io/contrib/bridges/otelslog"
)

// CapturedAttr marks spans and log records whose error the SDK already sent through the
// ingest API, so the server does not create a second error from the OTLP data.
const CapturedAttr = "vaka.error_captured"

// SlogOptions configures SlogHandler.
type SlogOptions struct {
	// BreadcrumbLevel is the lowest level recorded as a breadcrumb. Default slog.LevelInfo.
	BreadcrumbLevel slog.Level
	// ErrorLevel is the lowest level reported as a Vaka error. Default slog.LevelError.
	ErrorLevel slog.Level
	// DisableExport stops forwarding records to OpenTelemetry logs.
	DisableExport bool
}

// SlogHandler returns a log/slog handler that
//
//   - records each record as a breadcrumb on the context's scope,
//   - reports records at ERROR and above as Vaka errors, with stack, breadcrumbs, flags and
//     trace id (an `error`-valued attribute becomes the exception),
//   - exports every record to Vaka's log search through OpenTelemetry (when Init installed
//     the LoggerProvider), linked to the active trace,
//   - and passes records on to next, if not nil (for example a slog.TextHandler on stderr).
//
// Use the *Context logging methods (slog.InfoContext, logger.ErrorContext) so records join
// the request's scope and trace.
func SlogHandler(next slog.Handler) slog.Handler {
	return NewSlogHandler(next, SlogOptions{})
}

// NewSlogHandler is SlogHandler with options.
func NewSlogHandler(next slog.Handler, opts SlogOptions) slog.Handler {
	if opts.ErrorLevel == 0 {
		opts.ErrorLevel = slog.LevelError
	}
	h := &slogHandler{next: next, opts: opts}
	if !opts.DisableExport {
		h.otel = otelslog.NewHandler(sdkModule, otelslog.WithVersion(Version))
	}
	return h
}

type slogHandler struct {
	next  slog.Handler
	otel  slog.Handler
	opts  SlogOptions
	attrs []slog.Attr
	group string
}

func (h *slogHandler) Enabled(ctx context.Context, l slog.Level) bool {
	if l >= h.opts.BreadcrumbLevel || l >= h.opts.ErrorLevel {
		return true
	}
	if h.otel != nil && h.otel.Enabled(ctx, l) {
		return true
	}
	return h.next != nil && h.next.Enabled(ctx, l)
}

func slogLevel(l slog.Level) Level {
	switch {
	case l >= slog.LevelError+4:
		return LevelFatal
	case l >= slog.LevelError:
		return LevelError
	case l >= slog.LevelWarn:
		return LevelWarning
	case l >= slog.LevelInfo:
		return LevelInfo
	}
	return LevelDebug
}

func (h *slogHandler) Handle(ctx context.Context, r slog.Record) error {
	if ctx == nil {
		ctx = context.Background()
	}
	data := map[string]any{}
	var errVal error
	collect := func(a slog.Attr) bool {
		v := a.Value.Resolve()
		if e, ok := v.Any().(error); ok && errVal == nil {
			errVal = e
		}
		key := a.Key
		if h.group != "" {
			key = h.group + "." + key
		}
		data[key] = v.String()
		return true
	}
	for _, a := range h.attrs {
		collect(a)
	}
	r.Attrs(collect)

	c := CurrentClient()
	captured := false
	if r.Level >= h.opts.ErrorLevel && c.enabled() {
		opt := WithLevel(slogLevel(r.Level))
		if errVal != nil {
			id := c.captureLogError(ctx, errVal, r.Message, opt)
			captured = id != ""
		} else {
			captured = c.CaptureMessage(ctx, r.Message, slogLevel(r.Level), opt) != ""
		}
	}
	if r.Level >= h.opts.BreadcrumbLevel {
		ScopeFromContext(ctx).AddBreadcrumb(Breadcrumb{Timestamp: r.Time, Category: "log", Message: r.Message, Level: slogLevel(r.Level), Data: data})
	}
	var errs []error
	if h.otel != nil && h.otel.Enabled(ctx, r.Level) {
		rr := r
		if captured {
			rr = r.Clone()
			rr.AddAttrs(slog.Bool(CapturedAttr, true))
		}
		errs = append(errs, h.otel.Handle(ctx, rr))
	}
	if h.next != nil && h.next.Enabled(ctx, r.Level) {
		errs = append(errs, h.next.Handle(ctx, r))
	}
	return errors.Join(errs...)
}

func (h *slogHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	n := *h
	n.attrs = append(append([]slog.Attr(nil), h.attrs...), attrs...)
	if h.otel != nil {
		n.otel = h.otel.WithAttrs(attrs)
	}
	if h.next != nil {
		n.next = h.next.WithAttrs(attrs)
	}
	return &n
}

func (h *slogHandler) WithGroup(name string) slog.Handler {
	n := *h
	if h.group != "" {
		n.group = h.group + "." + name
	} else {
		n.group = name
	}
	if h.otel != nil {
		n.otel = h.otel.WithGroup(name)
	}
	if h.next != nil {
		n.next = h.next.WithGroup(name)
	}
	return &n
}

// captureLogError reports an error logged through slog; the log message becomes the
// event message.
func (c *Client) captureLogError(ctx context.Context, err error, msg string, opts ...EventOption) string {
	stack := trimLeading(c.Stacktrace(1))
	// Drop log/slog frames between the SDK and the caller.
	for len(stack) > 1 && stack[0].Module == "log/slog" {
		stack = stack[1:]
	}
	item := c.errorItem(ctx, LevelError, true, msg, c.exceptions(err, stack))
	return c.finish(item, opts)
}
