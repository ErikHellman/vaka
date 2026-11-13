import type { Context } from '@opentelemetry/api';
import { NodeSDK } from '@opentelemetry/sdk-node';
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto';
import { OTLPLogExporter } from '@opentelemetry/exporter-logs-otlp-proto';
import { BatchLogRecordProcessor, type LogRecordProcessor } from '@opentelemetry/sdk-logs';
import { BatchSpanProcessor, ParentBasedSampler, TraceIdRatioBasedSampler, type Span, type SpanProcessor } from '@opentelemetry/sdk-trace';
import { defaultResource, resourceFromAttributes } from '@opentelemetry/resources';
import { HttpInstrumentation } from '@opentelemetry/instrumentation-http';
import type { Instrumentation } from '@opentelemetry/instrumentation';
import { currentScope } from './scope';

export interface OtelConfig {
  endpoint: string;
  key: string;
  serviceName?: string;
  release?: string;
  environment?: string;
  tracesSampleRate: number;
  logs: boolean;
  instrumentations?: (Instrumentation | Instrumentation[])[];
  resourceAttributes?: Record<string, string | number | boolean>;
}

const headers = (key: string) => ({ 'x-vaka-key': key });

/** An OTLP/HTTP (protobuf) span exporter that sends to Vaka. Use it with your own NodeSDK. */
export function vakaSpanExporter(o: { endpoint: string; key: string }): OTLPTraceExporter {
  return new OTLPTraceExporter({ url: `${o.endpoint.replace(/\/+$/, '')}/ingest/otlp/v1/traces`, headers: headers(o.key) });
}

/** A batching log record processor that sends OpenTelemetry logs to Vaka. */
export function vakaLogRecordProcessor(o: { endpoint: string; key: string }): LogRecordProcessor {
  return new BatchLogRecordProcessor({ exporter: new OTLPLogExporter({ url: `${o.endpoint.replace(/\/+$/, '')}/ingest/otlp/v1/logs`, headers: headers(o.key) }) });
}

/**
 * Copies the current Vaka scope (user id, flags) onto every span as it starts, so traces can be
 * filtered by user and split by flag variant.
 */
export class VakaScopeSpanProcessor implements SpanProcessor {
  onStart(span: Span, _ctx: Context): void {
    const s = currentScope();
    if (s.user?.id) span.setAttribute('enduser.id', s.user.id);
    for (const k of Object.keys(s.flags)) span.setAttribute(`feature_flag.${k}`, s.flags[k]);
  }
  onEnd(): void {}
  forceFlush(): Promise<void> {
    return Promise.resolve();
  }
  shutdown(): Promise<void> {
    return Promise.resolve();
  }
}

export function startOtel(c: OtelConfig): NodeSDK {
  const attrs: Record<string, string | number | boolean> = { ...(c.resourceAttributes || {}) };
  if (c.serviceName) attrs['service.name'] = c.serviceName;
  if (c.release) attrs['service.version'] = c.release;
  if (c.environment) attrs['deployment.environment.name'] = c.environment;
  const rate = Number.isFinite(c.tracesSampleRate) ? Math.max(0, Math.min(1, c.tracesSampleRate)) : 1;
  const sdk = new NodeSDK({
    resource: defaultResource().merge(resourceFromAttributes(attrs)),
    serviceName: c.serviceName,
    sampler: new ParentBasedSampler({ root: new TraceIdRatioBasedSampler(rate) }),
    spanProcessors: [new VakaScopeSpanProcessor(), new BatchSpanProcessor({ exporter: vakaSpanExporter(c) })],
    logRecordProcessors: c.logs ? [vakaLogRecordProcessor(c)] : [],
    metricReaders: [],
    instrumentations: c.instrumentations || [new HttpInstrumentation()],
  });
  sdk.start();
  return sdk;
}
