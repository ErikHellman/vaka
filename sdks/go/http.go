package vaka

import (
	"fmt"
	"net/http"
	"time"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/trace"
)

// MiddlewareOptions configures Middleware.
type MiddlewareOptions struct {
	// Repanic re-panics after reporting, for outer middleware that handles panics itself.
	// Default false: the middleware answers 500.
	Repanic bool
	// DisableSpans skips the SERVER span (use when otelhttp or a framework already makes one).
	DisableSpans bool
}

// Middleware wraps an http.Handler: each request gets its own scope (see WithScope), a
// SERVER span that continues an incoming traceparent and is named after the route pattern
// (GET /carts/{id}), and panic reporting.
//
//	mux := http.NewServeMux()
//	mux.HandleFunc("GET /carts/{id}", getCart)
//	http.ListenAndServe(":8080", vaka.Middleware(mux))
func Middleware(next http.Handler) http.Handler {
	return MiddlewareWithOptions(next, MiddlewareOptions{})
}

// MiddlewareWithOptions is Middleware with options.
func MiddlewareWithOptions(next http.Handler, opts MiddlewareOptions) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := WithScope(r.Context())
		var span trace.Span
		if !opts.DisableSpans {
			ctx = otel.GetTextMapPropagator().Extract(ctx, propagation.HeaderCarrier(r.Header))
			ctx, span = Tracer().Start(ctx, r.Method, trace.WithSpanKind(trace.SpanKindServer),
				trace.WithAttributes(
					attribute.String("http.request.method", r.Method),
					attribute.String("url.path", r.URL.Path),
					attribute.String("server.address", r.Host),
				))
		}
		rec := &statusRecorder{ResponseWriter: w}
		r2 := r.WithContext(ctx)
		defer func() {
			v := recover()
			if v == http.ErrAbortHandler {
				panic(v)
			}
			if v != nil {
				CurrentClient().capturePanic(ctx, v, LevelError)
				if rec.status == 0 {
					rec.WriteHeader(http.StatusInternalServerError)
				}
			}
			if span != nil {
				if r2.Pattern != "" {
					span.SetName(r2.Pattern)
					span.SetAttributes(attribute.String("http.route", routeOf(r2.Pattern)))
				} else {
					span.SetName(r.Method)
				}
				status := rec.status
				if status == 0 {
					status = http.StatusOK
				}
				span.SetAttributes(attribute.Int("http.response.status_code", status))
				if v != nil {
					span.SetStatus(codes.Error, fmt.Sprint(v))
				} else if status >= 500 {
					span.SetStatus(codes.Error, "")
				}
				span.End()
			}
			if v != nil && opts.Repanic {
				panic(v)
			}
		}()
		next.ServeHTTP(rec, r2)
	})
}

// routeOf strips the method and host from a ServeMux pattern: "GET example.com/a/{id}" -> "/a/{id}".
func routeOf(pattern string) string {
	for i := 0; i < len(pattern); i++ {
		if pattern[i] == '/' {
			return pattern[i:]
		}
	}
	return pattern
}

type statusRecorder struct {
	http.ResponseWriter
	status int
}

func (s *statusRecorder) WriteHeader(code int) {
	if s.status == 0 {
		s.status = code
	}
	s.ResponseWriter.WriteHeader(code)
}

func (s *statusRecorder) Write(b []byte) (int, error) {
	if s.status == 0 {
		s.status = http.StatusOK
	}
	return s.ResponseWriter.Write(b)
}

func (s *statusRecorder) Flush() {
	if f, ok := s.ResponseWriter.(http.Flusher); ok {
		f.Flush()
	}
}

// Unwrap lets http.ResponseController reach the underlying writer.
func (s *statusRecorder) Unwrap() http.ResponseWriter { return s.ResponseWriter }

// Transport wraps an http.RoundTripper (nil = http.DefaultTransport) for outgoing requests:
// a CLIENT span, a traceparent header so the next service joins the trace, and an http
// breadcrumb on the request context's scope.
//
//	client := &http.Client{Transport: vaka.Transport(nil)}
func Transport(base http.RoundTripper) http.RoundTripper {
	if base == nil {
		base = http.DefaultTransport
	}
	return roundTripper{base}
}

type roundTripper struct{ base http.RoundTripper }

func (t roundTripper) RoundTrip(r *http.Request) (*http.Response, error) {
	ctx, span := Tracer().Start(r.Context(), r.Method+" "+r.URL.Host, trace.WithSpanKind(trace.SpanKindClient),
		trace.WithAttributes(
			attribute.String("http.request.method", r.Method),
			attribute.String("url.full", r.URL.Redacted()),
			attribute.String("server.address", r.URL.Hostname()),
		))
	defer span.End()
	r = r.Clone(ctx)
	otel.GetTextMapPropagator().Inject(ctx, propagation.HeaderCarrier(r.Header))
	start := time.Now()
	resp, err := t.base.RoundTrip(r)
	data := map[string]any{
		"method":      r.Method,
		"url":         r.URL.Redacted(),
		"duration_ms": time.Since(start).Milliseconds(),
	}
	level := LevelInfo
	if err != nil {
		span.SetStatus(codes.Error, err.Error())
		level = LevelError
	} else {
		data["status_code"] = resp.StatusCode
		span.SetAttributes(attribute.Int("http.response.status_code", resp.StatusCode))
		if resp.StatusCode >= 400 {
			span.SetStatus(codes.Error, "")
			level = LevelWarning
		}
	}
	ScopeFromContext(ctx).AddBreadcrumb(Breadcrumb{Category: "http", Message: r.Method + " " + r.URL.Redacted(), Level: level, Data: data})
	return resp, err
}
