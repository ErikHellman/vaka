package vaka

import (
	"context"
	"errors"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/exporters/otlp/otlplog/otlploghttp"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracehttp"
	"go.opentelemetry.io/otel/log/global"
	"go.opentelemetry.io/otel/propagation"
	sdklog "go.opentelemetry.io/otel/sdk/log"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/trace"
)

type otelProviders struct {
	tracer *sdktrace.TracerProvider
	logger *sdklog.LoggerProvider
}

// Resource returns the OpenTelemetry resource the SDK uses: service.name, service.version
// (the release) and deployment.environment.name, merged over the SDK defaults and
// OTEL_RESOURCE_ATTRIBUTES.
func (c *Client) Resource() *resource.Resource { return otelResource(c.cfg) }

func otelResource(cfg Config) *resource.Resource {
	attrs := []attribute.KeyValue{
		attribute.String("deployment.environment.name", cfg.Environment),
	}
	if cfg.ServiceName != "" {
		attrs = append(attrs, attribute.String("service.name", cfg.ServiceName))
	}
	if cfg.Release != "" {
		attrs = append(attrs, attribute.String("service.version", cfg.Release))
	}
	r, err := resource.Merge(resource.Default(), resource.NewSchemaless(attrs...))
	if err != nil {
		return resource.NewSchemaless(attrs...)
	}
	return r
}

func setupOTel(cfg Config) (*otelProviders, error) {
	p := &otelProviders{}
	if cfg.DisableTracing && cfg.DisableLogs {
		return p, nil
	}
	ctx := context.Background()
	headers := map[string]string{"x-vaka-key": cfg.Key}
	res := otelResource(cfg)
	if !cfg.DisableTracing {
		exp, err := otlptracehttp.New(ctx,
			otlptracehttp.WithEndpointURL(cfg.Endpoint+"/ingest/otlp/v1/traces"),
			otlptracehttp.WithHeaders(headers),
			otlptracehttp.WithHTTPClient(cfg.HTTPClient),
		)
		if err != nil {
			return nil, err
		}
		sampler := cfg.TracesSampler
		if sampler == nil {
			sampler = sdktrace.ParentBased(sdktrace.AlwaysSample())
		}
		p.tracer = sdktrace.NewTracerProvider(
			sdktrace.WithBatcher(exp),
			sdktrace.WithResource(res),
			sdktrace.WithSampler(sampler),
		)
		otel.SetTracerProvider(p.tracer)
		otel.SetTextMapPropagator(propagation.NewCompositeTextMapPropagator(propagation.TraceContext{}, propagation.Baggage{}))
	}
	if !cfg.DisableLogs {
		exp, err := otlploghttp.New(ctx,
			otlploghttp.WithEndpointURL(cfg.Endpoint+"/ingest/otlp/v1/logs"),
			otlploghttp.WithHeaders(headers),
			otlploghttp.WithHTTPClient(cfg.HTTPClient),
		)
		if err != nil {
			return nil, err
		}
		p.logger = sdklog.NewLoggerProvider(
			sdklog.WithProcessor(sdklog.NewBatchProcessor(exp)),
			sdklog.WithResource(res),
		)
		global.SetLoggerProvider(p.logger)
	}
	return p, nil
}

func (p *otelProviders) flush(ctx context.Context) error {
	var errs []error
	if p.tracer != nil {
		errs = append(errs, p.tracer.ForceFlush(ctx))
	}
	if p.logger != nil {
		errs = append(errs, p.logger.ForceFlush(ctx))
	}
	return errors.Join(errs...)
}

func (p *otelProviders) shutdown(ctx context.Context) error {
	var errs []error
	if p.tracer != nil {
		errs = append(errs, p.tracer.Shutdown(ctx))
	}
	if p.logger != nil {
		errs = append(errs, p.logger.Shutdown(ctx))
	}
	return errors.Join(errs...)
}

// Tracer returns an OpenTelemetry tracer from the global TracerProvider (the one Init
// installed, unless DisableTracing is set).
func Tracer() trace.Tracer { return otel.Tracer(sdkModule, trace.WithInstrumentationVersion(Version)) }

// StartSpan starts a span with the SDK's tracer. End it with span.End().
//
//	ctx, span := vaka.StartSpan(ctx, "import catalog")
//	defer span.End()
func StartSpan(ctx context.Context, name string, opts ...trace.SpanStartOption) (context.Context, trace.Span) {
	return Tracer().Start(ctx, name, opts...)
}
