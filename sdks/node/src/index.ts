/**
 * Vaka Node.js SDK: crash and error reporting, OpenTelemetry traces and logs, feature flags.
 *
 *   import * as Vaka from '@hellsoft/vaka-node';
 *   Vaka.init({ key: process.env.VAKA_KEY, endpoint: 'https://vaka.example.com', release: '1.2.3' });
 */
import { randomUUID } from 'node:crypto';
import { hostname, release as osRelease, type as osType } from 'node:os';
import { trace } from '@opentelemetry/api';
import type { NodeSDK } from '@opentelemetry/sdk-node';
import type { Instrumentation } from '@opentelemetry/instrumentation';
import { addSourceContext, parseStack, type Frame } from './stack';
import { currentScope, globalScope, withScope, _resetScopes, type Breadcrumb, type Level, type User, type Scope } from './scope';
import { Transport, type Item } from './transport';
import { startOtel, vakaLogRecordProcessor, vakaSpanExporter, VakaScopeSpanProcessor } from './otel';
import {
  installBreadcrumbs,
  installProcessHandlers,
  makeErrorHandler,
  makeFastifyPlugin,
  makeRequestHandler,
  nodeRejectionMode,
  uninstallIntegrations,
} from './integrations';

export type { Frame, Breadcrumb, Level, User, Scope, Item };
export type { ReqLike } from "./integrations";
export { parseStack, withScope, currentScope, vakaSpanExporter, vakaLogRecordProcessor, VakaScopeSpanProcessor };

export type Props = Record<string, string | number | boolean | null | undefined>;
export type FlagValue = boolean | string;

export interface Options {
  /** Project ingest key (vk_...). Default: `VAKA_KEY` or `VAKA_INGEST_KEY`. */
  key?: string;
  /** Vaka server URL, e.g. https://vaka.example.com. Default: `VAKA_ENDPOINT`. */
  endpoint?: string;
  /** App version. Default: `VAKA_RELEASE`, else `npm_package_version`. */
  release?: string;
  /** Default: `VAKA_ENVIRONMENT`, else `NODE_ENV`, else `production`. */
  environment?: string;
  /** OpenTelemetry `service.name`. Default: `OTEL_SERVICE_NAME`, else `npm_package_name`. */
  serviceName?: string;
  /**
   * Start the OpenTelemetry Node SDK and export traces and logs to Vaka (default true). Set false
   * when you run your own NodeSDK; add `vakaSpanExporter()` and `vakaLogRecordProcessor()` to it.
   */
  otel?: boolean;
  /** Fraction of new traces to record, 0..1 (default 1). Incoming `traceparent` decisions win. */
  tracesSampleRate?: number;
  /** Export OpenTelemetry log records to Vaka (default true). */
  logs?: boolean;
  /**
   * OpenTelemetry instrumentations (default: HTTP server and client only). Pass
   * `getNodeAutoInstrumentations()` from `@opentelemetry/auto-instrumentations-node` for Express,
   * Fastify, databases, queues and more.
   */
  instrumentations?: (Instrumentation | Instrumentation[])[];
  /** Extra OpenTelemetry resource attributes. */
  resourceAttributes?: Record<string, string | number | boolean>;
  /** Report uncaught exceptions and unhandled rejections (default true). */
  captureUnhandled?: boolean;
  /**
   * After an unhandled rejection: `strict` exits with code 1 like Node does by default, `warn`
   * prints a warning and keeps running, `none` only reports. Default: follows
   * `--unhandled-rejections`, else `strict`.
   */
  unhandledRejection?: 'strict' | 'warn' | 'none';
  /** Exit with code 1 after reporting an uncaught exception, as Node does (default true). */
  exitOnUncaught?: boolean;
  /** How long to wait for delivery before exiting on a crash (default 2000 ms). */
  shutdownTimeoutMs?: number;
  /** Record console.warn/error and outgoing HTTP requests as breadcrumbs (default true). */
  breadcrumbs?: boolean;
  /** Maximum breadcrumbs kept per scope (default 100). */
  maxBreadcrumbs?: number;
  /** Read source lines around in-app frames from disk (default true). */
  sourceContext?: boolean;
  /** Fraction of errors to send, 0..1 (default 1). */
  errorSampleRate?: number;
  /** Errors whose message matches are dropped. */
  ignoreErrors?: (string | RegExp)[];
  /** Capture errors with these HTTP statuses from the middleware (default: status >= 500). */
  shouldCaptureStatus?: (status: number) => boolean;
  /** Cache flag evaluations per identity for this long (default 30000 ms; 0 disables). */
  flagsCacheTtlMs?: number;
  /** Modify or drop (return null) an item before it is sent. */
  beforeSend?: (item: Item) => Item | null;
  /** How often queued items are sent (default 2000 ms). Errors go out at once. */
  flushIntervalMs?: number;
  /** Log SDK activity to stderr. */
  debug?: boolean;
}

export interface CaptureContext {
  level?: Level;
  tags?: Record<string, string>;
  extra?: Record<string, unknown>;
  handled?: boolean;
  fingerprint?: string[];
  user?: User;
}

export interface FlagContext {
  /** Stable id for bucketing; defaults to the scope's user id. */
  distinctId?: string;
  userId?: string;
  /** Properties targeting rules can match, e.g. `{ plan: 'pro' }`. */
  properties?: Props;
}

export const SDK = { name: 'vaka-node', version: '0.2.0' };

type Resolved = Required<Pick<Options, 'key' | 'endpoint' | 'environment'>> & Options;

let opts: Resolved | undefined;
let transport: Transport | undefined;
let otelSdk: NodeSDK | undefined;
const processId = randomUUID();

function log(...a: unknown[]) {
  if (opts?.debug) process.stderr.write(`[vaka] ${a.map((x) => (typeof x === 'string' ? x : JSON.stringify(x))).join(' ')}\n`);
}

function env(...names: string[]): string | undefined {
  for (const n of names) {
    const v = process.env[n];
    if (v) return v;
  }
  return undefined;
}

/** Initialize the SDK. Call once, as early as possible (before importing your HTTP framework). */
export function init(options: Options = {}): void {
  if (opts) return;
  options = Object.fromEntries(Object.entries(options).filter(([, v]) => v !== undefined)) as Options;
  const key = options.key || env('VAKA_KEY', 'VAKA_INGEST_KEY');
  const endpoint = (options.endpoint || env('VAKA_ENDPOINT') || '').replace(/\/+$/, '');
  if (!key || !endpoint) {
    process.emitWarning('[vaka] init needs { key, endpoint } (or VAKA_KEY and VAKA_ENDPOINT); SDK disabled');
    return;
  }
  opts = {
    otel: true,
    tracesSampleRate: 1,
    logs: true,
    captureUnhandled: true,
    exitOnUncaught: true,
    shutdownTimeoutMs: 2000,
    breadcrumbs: true,
    maxBreadcrumbs: 100,
    sourceContext: true,
    errorSampleRate: 1,
    flagsCacheTtlMs: 30000,
    flushIntervalMs: 2000,
    ...options,
    key,
    endpoint,
    release: options.release || env('VAKA_RELEASE', 'npm_package_version'),
    environment: options.environment || env('VAKA_ENVIRONMENT', 'NODE_ENV') || 'production',
    serviceName: options.serviceName || env('OTEL_SERVICE_NAME', 'npm_package_name'),
  };
  const o = opts;
  transport = new Transport({ endpoint, key, sdk: SDK, flushIntervalMs: o.flushIntervalMs!, log });
  if (o.breadcrumbs) installBreadcrumbs(endpoint, (b) => addBreadcrumb(b));
  if (o.captureUnhandled) {
    installProcessHandlers({
      capture: (err, fatal, mechanism) => {
        captureException(err, { handled: false, level: fatal ? 'fatal' : 'error', tags: { mechanism } }, fatal);
      },
      flush: (ms) => close(ms),
      unhandledRejection: o.unhandledRejection || nodeRejectionMode(),
      exitOnUncaught: o.exitOnUncaught!,
      shutdownTimeoutMs: o.shutdownTimeoutMs!,
    });
  }
  if (o.otel) {
    try {
      otelSdk = startOtel({
        endpoint,
        key,
        serviceName: o.serviceName,
        release: o.release,
        environment: o.environment,
        tracesSampleRate: o.tracesSampleRate!,
        logs: o.logs!,
        instrumentations: o.instrumentations,
        resourceAttributes: o.resourceAttributes,
      });
    } catch (e) {
      log('OpenTelemetry setup failed:', String(e));
    }
  }
  process.once('beforeExit', () => void flush());
  log('initialized', endpoint, o.release || '(no release)', o.environment);
}

/** True after a successful init(). */
export function isInitialized(): boolean {
  return !!opts;
}

// ---------------------------------------------------------------------------
// Scope helpers
// ---------------------------------------------------------------------------

/** Set the user for the current scope (request inside middleware, else global). Opt-in. */
export function setUser(user: User | string | null): void {
  currentScope().user = user === null ? undefined : typeof user === 'string' ? { id: user } : user;
  const span = trace.getActiveSpan();
  const id = currentScope().user?.id;
  if (span && id) span.setAttribute('enduser.id', id);
}

export function setTag(key: string, value: string): void {
  currentScope().tags[key] = String(value);
}

export function setTags(tags: Record<string, string>): void {
  for (const k of Object.keys(tags)) setTag(k, tags[k]);
}

/** Attach structured context (e.g. `setContext('order', { id: 42 })`) to errors in this scope. */
export function setContext(name: string, value: Record<string, unknown> | null): void {
  if (value === null) delete currentScope().contexts[name];
  else currentScope().contexts[name] = value;
}

export function addBreadcrumb(b: Breadcrumb): void {
  currentScope().addBreadcrumb(b, opts?.maxBreadcrumbs);
}

/** The active OpenTelemetry trace id, if any. */
export function traceId(): string | undefined {
  const ctx = trace.getActiveSpan()?.spanContext();
  return ctx && ctx.traceId !== '00000000000000000000000000000000' ? ctx.traceId : undefined;
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

function exceptionChain(error: unknown) {
  const out: { type: string; value: string; module?: string; stacktrace: Frame[] }[] = [];
  let e: unknown = error;
  const seen = new Set<unknown>();
  for (let i = 0; i < 6 && e !== undefined && e !== null && !seen.has(e); i++) {
    seen.add(e);
    if (e instanceof Error || (typeof e === 'object' && 'message' in (e as object))) {
      const err = e as Error & { cause?: unknown; code?: string };
      let frames = parseStack(err.stack);
      if (opts?.sourceContext) frames = addSourceContext(frames);
      const type = err.name && err.name !== 'Error' ? err.name : err.constructor?.name && err.constructor.name !== 'Object' ? err.constructor.name : 'Error';
      out.push({ type: err.code && type === 'Error' ? `Error [${err.code}]` : type, value: String(err.message ?? ''), stacktrace: frames });
      e = err.cause;
    } else {
      out.push({ type: 'Error', value: typeof e === 'string' ? e : safeJson(e), stacktrace: [] });
      break;
    }
  }
  return out;
}

function safeJson(v: unknown): string {
  try {
    return JSON.stringify(v)?.slice(0, 1000) ?? String(v);
  } catch {
    return String(v);
  }
}

function ignored(error: unknown): boolean {
  const pats = opts?.ignoreErrors;
  if (!pats || !pats.length) return false;
  const msg = String((error as Error)?.message ?? error);
  return pats.some((p) => (typeof p === 'string' ? msg.includes(p) : p.test(msg)));
}

function baseItem(scope: Scope, ctx: CaptureContext) {
  const span = trace.getActiveSpan()?.spanContext();
  const tid = span && span.traceId !== '00000000000000000000000000000000' ? span.traceId : undefined;
  const contexts: Record<string, unknown> = {
    runtime: { name: 'node', version: process.version },
    os: { name: osType(), version: osRelease() },
    ...scope.contexts,
  };
  if (ctx.extra) contexts.extra = ctx.extra;
  if (scope.request) contexts.request = scope.request;
  if (tid) contexts.trace = { trace_id: tid, span_id: span!.spanId };
  const user = ctx.user || scope.user;
  const tags: Record<string, string> = { server_name: hostname(), ...scope.tags, ...(ctx.tags || {}) };
  if (scope.request?.method && scope.request.route) tags.transaction = `${scope.request.method} ${scope.request.route}`;
  if (opts?.serviceName) tags.service = opts.serviceName;
  return {
    event_id: randomUUID(),
    timestamp: new Date().toISOString(),
    platform: 'node',
    release: opts?.release,
    environment: opts?.environment,
    breadcrumbs: scope.breadcrumbs.slice(),
    tags,
    contexts,
    user: user ? { id: user.id } : undefined,
    user_id: user?.id,
    url: scope.request?.url,
    trace_id: tid,
    flags: Object.keys(scope.flags).length ? { ...scope.flags } : undefined,
  };
}

/**
 * Report an error. Returns the event id, or undefined when it was dropped. `fatal` marks a crash
 * (the process is about to exit).
 */
export function captureException(error: unknown, ctx: CaptureContext = {}, fatal = false): string | undefined {
  if (!opts || !transport) return;
  if (ignored(error)) return;
  if (!fatal && (opts.errorSampleRate ?? 1) < 1 && Math.random() >= (opts.errorSampleRate ?? 1)) return;
  const scope = currentScope();
  const item: Item = {
    type: 'error',
    ...baseItem(scope, ctx),
    level: ctx.level || (fatal ? 'fatal' : 'error'),
    handled: ctx.handled !== false,
    fatal,
    exceptions: exceptionChain(error),
    fingerprint: ctx.fingerprint,
  };
  const id = item.event_id as string;
  if (!enqueue(item, true)) return;
  // The server skips OTLP-derived errors for spans marked like this, so the error isn't counted twice.
  trace.getActiveSpan()?.setAttribute('vaka.error_captured', true);
  scope.addBreadcrumb({ category: 'error', level: 'error', message: String((error as Error)?.message ?? error).slice(0, 300) }, opts.maxBreadcrumbs);
  return id;
}

/** Alias of captureException, matching the browser SDK. */
export const captureError = captureException;

/** Report a message as an error-like event. */
export function captureMessage(message: string, level: Level = 'info', ctx: CaptureContext = {}): string | undefined {
  if (!opts || !transport) return;
  const item: Item = { type: 'error', ...baseItem(currentScope(), ctx), level, message, fingerprint: ctx.fingerprint };
  return enqueue(item, level === 'error' || level === 'fatal') ? (item.event_id as string) : undefined;
}

// ---------------------------------------------------------------------------
// Analytics events
// ---------------------------------------------------------------------------

/** Record a custom server-side event (e.g. `track('order_paid', { amount: 49 })`). */
export function track(name: string, props?: Props, o: { userId?: string; url?: string } = {}): void {
  if (!opts || !name) return;
  const scope = currentScope();
  enqueue({
    type: 'event',
    name,
    timestamp: new Date().toISOString(),
    props: clean(props),
    user_id: o.userId || scope.user?.id,
    anon_id: o.userId || scope.user?.id ? undefined : processId,
    url: o.url,
    release: opts.release,
    environment: opts.environment,
    flags: Object.keys(scope.flags).length ? { ...scope.flags } : undefined,
  });
}

function clean(props?: Props): Record<string, string | number | boolean> | undefined {
  if (!props) return undefined;
  const out: Record<string, string | number | boolean> = {};
  for (const k of Object.keys(props)) {
    const v = props[k];
    if (v !== null && v !== undefined) out[k] = v;
  }
  return Object.keys(out).length ? out : undefined;
}

function enqueue(item: Item, urgent = false): boolean {
  if (!opts || !transport) return false;
  for (const k of Object.keys(item)) if (item[k] === undefined) delete item[k];
  let final: Item | null = item;
  if (opts.beforeSend) {
    try {
      final = opts.beforeSend(item);
    } catch (e) {
      log('beforeSend threw:', String(e));
    }
  }
  if (!final) return false;
  transport.send(final, urgent);
  return true;
}

// ---------------------------------------------------------------------------
// Feature flags
// ---------------------------------------------------------------------------

const flagCache = new Map<string, { at: number; flags: Record<string, FlagValue> }>();

/**
 * Evaluate all flags for one person. The values are also attached to the current scope, so
 * errors, events and spans in this request carry them as exposures.
 */
export async function getFlags(ctx: FlagContext = {}): Promise<Record<string, FlagValue>> {
  if (!opts || !transport || transport.disabled) return {};
  const scope = currentScope();
  const userId = ctx.userId || scope.user?.id;
  const distinct = ctx.distinctId || userId || processId;
  const body = { distinct_id: distinct, user_id: userId, environment: opts.environment, properties: clean(ctx.properties) };
  const cacheKey = JSON.stringify(body);
  const ttl = opts.flagsCacheTtlMs ?? 30000;
  const hit = flagCache.get(cacheKey);
  let flags: Record<string, FlagValue>;
  if (hit && ttl > 0 && Date.now() - hit.at < ttl) {
    flags = hit.flags;
  } else {
    const { context } = await import('@opentelemetry/api');
    const { suppressTracing } = await import('@opentelemetry/core');
    const r = await context.with(suppressTracing(context.active()), () =>
      fetch(`${opts!.endpoint}/ingest/v1/flags`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-Vaka-Key': opts!.key },
        body: JSON.stringify(body),
        signal: AbortSignal.timeout(5000),
      }),
    );
    if (r.status !== 200) throw new Error(`vaka flags: HTTP ${r.status}`);
    flags = ((await r.json()) as { flags?: Record<string, FlagValue> }).flags || {};
    if (ttl > 0) {
      if (flagCache.size >= 1000) flagCache.delete(flagCache.keys().next().value as string);
      flagCache.set(cacheKey, { at: Date.now(), flags });
    }
  }
  for (const k of Object.keys(flags)) scope.flags[k] = String(flags[k]);
  const span = trace.getActiveSpan();
  if (span) for (const k of Object.keys(flags)) span.setAttribute(`feature_flag.${k}`, String(flags[k]));
  return flags;
}

/** One flag's value: `true`/`false`, or the variant key. Returns `fallback` when unknown or on failure. */
export async function getFlag(key: string, fallback: FlagValue = false, ctx: FlagContext = {}): Promise<FlagValue> {
  try {
    const flags = await getFlags(ctx);
    return key in flags ? flags[key] : fallback;
  } catch (e) {
    log('flag evaluation failed:', String(e));
    return fallback;
  }
}

/** True when a flag is on (any variant). */
export async function isEnabled(key: string, ctx: FlagContext = {}): Promise<boolean> {
  const v = await getFlag(key, false, ctx);
  return v !== false && v !== 'false';
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

function middlewareCapture(err: unknown, c: { handled: boolean; mechanism: string }) {
  captureException(err, { handled: c.handled, tags: { mechanism: c.mechanism } });
}
const shouldCapture = (status: number) => (opts?.shouldCaptureStatus ? opts.shouldCaptureStatus(status) : status >= 500);

/** Express/Connect: isolates breadcrumbs, user and tags per request. `app.use(Vaka.requestHandler())` first. */
export function requestHandler() {
  return makeRequestHandler();
}

/** Express: reports errors passed to `next(err)`. `app.use(Vaka.errorHandler())` after your routes. */
export function errorHandler() {
  return makeErrorHandler(middlewareCapture, shouldCapture);
}

/** Express: add both middlewares' error half to an existing app (call after routes). */
export function setupExpressErrorHandler(app: { use: (...a: unknown[]) => unknown }): void {
  app.use(errorHandler());
}

/** Fastify: `await app.register(Vaka.fastifyPlugin)`. */
export const fastifyPlugin = makeFastifyPlugin(middlewareCapture, shouldCapture);

/** Wrap any `(req, res)` handler (node:http, serverless) so it gets a scope and errors are reported. */
export function wrapHandler<A extends unknown[], R>(fn: (...args: A) => R): (...args: A) => R {
  return (...args: A) =>
    withScope((scope) => {
      scope.breadcrumbs = [];
      try {
        const r = fn(...args);
        if (r && typeof (r as unknown as Promise<unknown>).then === 'function') {
          return (r as unknown as Promise<unknown>).catch((e) => {
            captureException(e, { handled: false, tags: { mechanism: 'wrapHandler' } });
            throw e;
          }) as unknown as R;
        }
        return r;
      } catch (e) {
        captureException(e, { handled: false, tags: { mechanism: 'wrapHandler' } });
        throw e;
      }
    });
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/** Send queued items. Resolves true when everything was delivered within `timeoutMs`. */
export async function flush(timeoutMs = 5000): Promise<boolean> {
  if (!transport) return true;
  return transport.flush(timeoutMs);
}

/** Flush errors and events, then shut down OpenTelemetry (flushing spans and logs). */
export async function close(timeoutMs = 5000): Promise<boolean> {
  const sent = await flush(timeoutMs);
  if (otelSdk) {
    const sdk = otelSdk;
    otelSdk = undefined;
    let t: NodeJS.Timeout | undefined;
    await Promise.race([
      sdk.shutdown().catch(() => undefined),
      new Promise((r) => {
        t = setTimeout(r, timeoutMs);
        t.unref?.();
      }),
    ]);
    if (t) clearTimeout(t);
  }
  return sent;
}

/** For tests: reset all state and remove hooks. */
export async function _reset(): Promise<void> {
  uninstallIntegrations();
  if (otelSdk) await otelSdk.shutdown().catch(() => undefined);
  otelSdk = undefined;
  opts = undefined;
  transport = undefined;
  flagCache.clear();
  _resetScopes();
}

export { globalScope as _globalScope };
