/**
 * Vaka browser SDK: crash reporting and cookieless analytics.
 *
 *   import * as Vaka from 'vaka-browser';
 *   Vaka.init({ key: 'vk_...', endpoint: 'https://vaka.example.com', release: '1.2.3' });
 *   Vaka.track('signup', { plan: 'pro' });
 */
import { parseStack, Frame } from './stack';

export type { Frame };
export type Level = 'fatal' | 'error' | 'warning' | 'info';
export type Props = Record<string, string | number | boolean | null | undefined>;

export interface Options {
  /** Project ingest key (vk_...). */
  key: string;
  /** Vaka server URL, e.g. https://vaka.example.com */
  endpoint: string;
  /** App version; must match the release used when uploading source maps. */
  release?: string;
  environment?: string;
  /** Send a pageview on load and on SPA navigation (default true). */
  autoPageviews?: boolean;
  /** Treat hash changes as navigation (default false). */
  hashRouting?: boolean;
  /** Capture uncaught errors and unhandled rejections (default true). */
  captureErrors?: boolean;
  /** Record breadcrumbs for clicks, navigation, console and fetch requests (default true). */
  breadcrumbs?: boolean;
  /**
   * Group events and errors of one visit (per browser tab, ends after 30 minutes of inactivity) under a
   * session id kept in sessionStorage, so timelines before a crash can be rebuilt (default true).
   */
  sessions?: boolean;
  /**
   * Fraction of page loads to trace, 0..1 (default 0: no tracing). Traced page loads send a
   * `pageload` span, an `http.client` span per fetch and a `web.vitals` span (LCP, INP, CLS, FCP,
   * TTFB) when the page is hidden. Errors carry the current trace id.
   */
  tracesSampleRate?: number;
  /**
   * Add a W3C `traceparent` header to fetch requests whose URL starts with one of these
   * (default: same-origin requests only). Your backend can continue the trace with it.
   */
  tracePropagationTargets?: (string | RegExp)[];
  /** Fraction of error events to send, 0..1 (default 1). */
  errorSampleRate?: number;
  /** Don't send anything from localhost (default false). */
  ignoreLocalhost?: boolean;
  /** Modify or drop (return null) an item before it is sent. */
  beforeSend?: (item: Item) => Item | null;
  /** Log SDK activity to the console. */
  debug?: boolean;
}

export interface Breadcrumb {
  timestamp?: string;
  category?: string;
  message?: string;
  level?: Level;
  data?: Record<string, unknown>;
}

export interface Item {
  type: 'error' | 'event' | 'session' | 'span';
  [k: string]: unknown;
}

export interface CaptureContext {
  level?: Level;
  tags?: Record<string, string>;
  extra?: Record<string, unknown>;
  handled?: boolean;
  fingerprint?: string[];
}

const SDK = { name: 'vaka-browser', version: '0.1.0' };
const MAX_BREADCRUMBS = 50;
const FLUSH_MS = 5000;
const MAX_QUEUE = 20;

let opts: Required<Pick<Options, 'key' | 'endpoint'>> & Options;
let started = false;
let queue: Item[] = [];
let timer: ReturnType<typeof setTimeout> | undefined;
let crumbs: Breadcrumb[] = [];
let userId: string | undefined;
let tags: Record<string, string> = {};
let lastUrl = '';
let backoffUntil = 0;
let disabled = false;

function log(...a: unknown[]) {
  if (opts && opts.debug && typeof console !== 'undefined') console.log('[vaka]', ...a);
}

const now = () => new Date().toISOString();
const hasWindow = typeof window !== 'undefined';

function uuid(): string {
  const c = hasWindow ? window.crypto : undefined;
  if (c && 'randomUUID' in c) return (c as Crypto).randomUUID();
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (ch) => {
    const r = (Math.random() * 16) | 0;
    return (ch === 'x' ? r : (r & 0x3) | 0x8).toString(16);
  });
}

/** Initialize the SDK. Safe to call once; later calls are ignored. */
export function init(options: Options): void {
  if (started) return;
  if (!options || !options.key || !options.endpoint) {
    if (typeof console !== 'undefined') console.warn('[vaka] init needs { key, endpoint }');
    return;
  }
  opts = { autoPageviews: true, captureErrors: true, breadcrumbs: true, errorSampleRate: 1, environment: 'production', ...options };
  opts.endpoint = opts.endpoint.replace(/\/+$/, '');
  started = true;
  if (!hasWindow) return;
  if (opts.ignoreLocalhost && /^(localhost|127\.|\[::1\])/.test(location.hostname)) {
    disabled = true;
    log('disabled on localhost');
    return;
  }
  if ((opts.tracesSampleRate ?? 0) > 0 && Math.random() < (opts.tracesSampleRate ?? 0)) installTracing();
  if (opts.captureErrors) installErrorHandlers();
  if (opts.breadcrumbs) installBreadcrumbs();
  if (opts.autoPageviews) installPageviews();
  const flushOnHide = () => flush(true);
  window.addEventListener('pagehide', flushOnHide);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden') flushOnHide();
  });
  log('initialized', opts.endpoint);
}

/** Record a pageview. Called automatically unless autoPageviews is false. */
export function pageview(url?: string, props?: Props): void {
  if (!hasWindow) return;
  const href = url || location.href;
  enqueue({
    type: 'event',
    name: 'pageview',
    url: href,
    referrer: lastUrl ? '' : document.referrer || '',
    props: clean(props),
    ...common(),
  });
  lastUrl = href;
}

/** Record a custom event. */
export function track(name: string, props?: Props): void {
  if (!name) return;
  enqueue({ type: 'event', name, url: hasWindow ? location.href : undefined, props: clean(props), ...common() });
}

/** Opt-in user identification. Pass null to clear. */
export function identify(id: string | null): void {
  userId = id || undefined;
}

export function setTag(key: string, value: string): void {
  tags[key] = String(value);
}

export function addBreadcrumb(b: Breadcrumb): void {
  crumbs.push({ timestamp: now(), level: 'info', ...b });
  if (crumbs.length > MAX_BREADCRUMBS) crumbs = crumbs.slice(-MAX_BREADCRUMBS);
}

/** Report a caught error. */
export function captureError(error: unknown, ctx: CaptureContext = {}): string | undefined {
  if (!started) return;
  if ((opts.errorSampleRate ?? 1) < 1 && Math.random() > (opts.errorSampleRate ?? 1)) return;
  const eventId = uuid();
  enqueue({
    type: 'error',
    event_id: eventId,
    timestamp: now(),
    platform: 'javascript',
    level: ctx.level || 'error',
    handled: ctx.handled !== false,
    exceptions: exceptionChain(error),
    breadcrumbs: crumbs.slice(),
    tags: { ...tags, ...(ctx.tags || {}) },
    contexts: ctx.extra ? { extra: ctx.extra } : undefined,
    fingerprint: ctx.fingerprint,
    url: hasWindow ? location.href : undefined,
    trace_id: trace?.id,
    ...common(),
  });
  addBreadcrumb({ category: 'error', message: String((error as Error)?.message || error), level: 'error' });
  return eventId;
}

/** Report a message as an error-like event. */
export function captureMessage(message: string, level: Level = 'info'): string | undefined {
  if (!started) return;
  const eventId = uuid();
  enqueue({ type: 'error', event_id: eventId, timestamp: now(), platform: 'javascript', level, message, breadcrumbs: crumbs.slice(), tags: { ...tags }, url: hasWindow ? location.href : undefined, ...common() });
  return eventId;
}

/** Send everything queued now. Returns when the request finished. */
export function flush(useBeacon = false): Promise<void> {
  if (timer) {
    clearTimeout(timer);
    timer = undefined;
  }
  if (!queue.length || !started || disabled) return Promise.resolve();
  if (Date.now() < backoffUntil && !useBeacon) {
    schedule();
    return Promise.resolve();
  }
  const items = queue.splice(0, 1000);
  const body = JSON.stringify({ sdk: SDK, items });
  const url = `${opts.endpoint}/ingest/v1/batch`;
  if (useBeacon && hasWindow && navigator.sendBeacon) {
    const ok = navigator.sendBeacon(`${url}?key=${encodeURIComponent(opts.key)}`, new Blob([body], { type: 'text/plain' }));
    if (ok) return Promise.resolve();
  }
  if (typeof fetch === 'undefined') return Promise.resolve();
  return origFetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'text/plain', 'X-Vaka-Key': opts.key },
    body,
    keepalive: body.length < 60000,
  })
    .then((r) => {
      if (r.status === 429 || r.status >= 500) {
        retry(items);
      } else if (r.status === 401) {
        disabled = true;
        log('ingest key rejected; disabling');
      } else {
        log('sent', items.length, 'items', r.status);
      }
    })
    .catch(() => retry(items));
}

function retry(items: Item[]) {
  backoffUntil = Date.now() + Math.min(300000, (backoffUntil > Date.now() ? (backoffUntil - Date.now()) * 2 : 5000));
  queue = items.concat(queue).slice(0, 500);
  schedule();
}

const SESSION_IDLE_MS = 30 * 60 * 1000;
let memSession: { id: string; last: number } | undefined;

/** The current visit's session id, rotated after 30 minutes without activity. */
export function sessionId(): string | undefined {
  if (!opts || opts.sessions === false) return undefined;
  const t = Date.now();
  let s = memSession;
  if (!s && hasWindow) {
    try {
      const raw = window.sessionStorage.getItem('vaka_sid');
      if (raw) s = JSON.parse(raw);
    } catch {
      /* storage blocked */
    }
  }
  if (!s || typeof s.id !== 'string' || t - s.last > SESSION_IDLE_MS) s = { id: uuid(), last: t };
  s.last = t;
  memSession = s;
  if (hasWindow) {
    try {
      window.sessionStorage.setItem('vaka_sid', JSON.stringify(s));
    } catch {
      /* storage blocked */
    }
  }
  return s.id;
}

function common() {
  return { release: opts.release, environment: opts.environment, user_id: userId, session_id: sessionId() };
}

function clean(props?: Props): Record<string, string | number | boolean> | undefined {
  if (!props) return undefined;
  const out: Record<string, string | number | boolean> = {};
  for (const k of Object.keys(props)) {
    const v = props[k];
    if (v !== null && v !== undefined) out[k] = v;
  }
  return out;
}

function enqueue(item: Item) {
  if (!started || disabled) return;
  for (const k of Object.keys(item)) if (item[k] === undefined) delete item[k];
  const final = opts.beforeSend ? opts.beforeSend(item) : item;
  if (!final) return;
  queue.push(final);
  if (queue.length >= MAX_QUEUE || final.type === 'error') {
    // Errors go out quickly: the page may be about to break.
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => flush(), final.type === 'error' ? 200 : 0);
  } else {
    schedule();
  }
}

function schedule() {
  if (!timer) timer = setTimeout(() => flush(), FLUSH_MS);
}

function exceptionChain(error: unknown) {
  const out: { type: string; value: string; stacktrace: Frame[] }[] = [];
  let e: unknown = error;
  const origin = hasWindow ? location.origin : undefined;
  for (let i = 0; i < 5 && e; i++) {
    if (e instanceof Error || (typeof e === 'object' && e !== null && 'message' in e)) {
      const err = e as Error & { cause?: unknown };
      out.push({ type: err.name || 'Error', value: String(err.message || ''), stacktrace: parseStack(err.stack, origin) });
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
    return JSON.stringify(v).slice(0, 500);
  } catch {
    return String(v);
  }
}

function installErrorHandlers() {
  window.addEventListener('error', (ev: ErrorEvent) => {
    if (!ev.error && !ev.message) return; // resource load errors
    const err = ev.error || { name: 'Error', message: ev.message, stack: ev.filename ? `at ${ev.filename}:${ev.lineno}:${ev.colno}` : '' };
    captureError(err, { handled: false, level: 'fatal' });
  });
  window.addEventListener('unhandledrejection', (ev: PromiseRejectionEvent) => {
    const reason = ev.reason;
    captureError(reason instanceof Error ? reason : { name: 'UnhandledRejection', message: safeJson(reason) }, { handled: false });
  });
}

let origFetch: typeof fetch = typeof fetch !== 'undefined' ? fetch.bind(globalThis) : (undefined as unknown as typeof fetch);

function describe(el: Element | null): string {
  if (!el || !el.tagName) return '';
  let s = el.tagName.toLowerCase();
  if (el.id) s += '#' + el.id;
  const cls = typeof el.className === 'string' ? el.className.trim().split(/\s+/).slice(0, 2).join('.') : '';
  if (cls) s += '.' + cls;
  const text = (el.textContent || '').trim().slice(0, 30);
  return text ? `${s} "${text}"` : s;
}

function installBreadcrumbs() {
  document.addEventListener(
    'click',
    (ev) => addBreadcrumb({ category: 'ui.click', message: describe(ev.target as Element) }),
    true,
  );
  for (const level of ['warn', 'error'] as const) {
    const orig = console[level];
    console[level] = function (...args: unknown[]) {
      addBreadcrumb({ category: 'console', level: level === 'warn' ? 'warning' : 'error', message: args.map((a) => (typeof a === 'string' ? a : safeJson(a))).join(' ').slice(0, 300) });
      return orig.apply(console, args);
    };
  }
  if (typeof fetch !== 'undefined') {
    const f = fetch;
    window.fetch = function (input: RequestInfo | URL, init?: RequestInit) {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.href : (input as Request).url;
      const method = (init && init.method) || (typeof input === 'object' && 'method' in input ? (input as Request).method : 'GET');
      const started = Date.now();
      const own = url.indexOf(opts.endpoint) === 0;
      const span = !own && trace ? startSpan({ name: `${method.toUpperCase()} ${url}`, op: 'http.client', kind: 'client' }) : undefined;
      if (span && propagate(url)) {
        const h = new Headers((init && init.headers) || (typeof input === 'object' && 'headers' in input ? (input as Request).headers : undefined));
        if (!h.has('traceparent')) h.set('traceparent', `00-${span.traceId}-${span.spanId}-01`);
        init = { ...(init || {}), headers: h };
      }
      return f.call(window, input, init).then(
        (r) => {
          span?.end(r.status >= 500 ? 'error' : 'ok', { method: method.toUpperCase(), status_code: r.status });
          if (!own)
            addBreadcrumb({
              category: 'http',
              level: r.status >= 400 ? 'warning' : 'info',
              message: `${method} ${url} ${r.status}`,
              data: { method, url, status_code: r.status, duration_ms: Date.now() - started },
            });
          return r;
        },
        (e: unknown) => {
          span?.end('error', { method: method.toUpperCase() });
          if (!own) addBreadcrumb({ category: 'http', level: 'error', message: `${method} ${url} failed`, data: { method, url, duration_ms: Date.now() - started } });
          throw e;
        },
      );
    };
  }
}

function installPageviews() {
  const onNav = () => {
    const href = opts.hashRouting ? location.href : location.href.split('#')[0];
    const last = opts.hashRouting ? lastUrl : lastUrl.split('#')[0];
    if (href === last) return;
    addBreadcrumb({ category: 'navigation', message: `${lastUrl ? new URL(lastUrl).pathname : ''} -> ${location.pathname}` });
    pageview(href);
  };
  for (const m of ['pushState', 'replaceState'] as const) {
    const orig = history[m];
    history[m] = function (this: History, ...args: Parameters<History['pushState']>) {
      const r = orig.apply(this, args);
      setTimeout(onNav, 0);
      return r;
    } as History['pushState'];
  }
  window.addEventListener('popstate', onNav);
  if (opts.hashRouting) window.addEventListener('hashchange', onNav);
  if ((document.visibilityState as string) === 'prerender') {
    document.addEventListener('visibilitychange', onNav, { once: true });
  } else {
    onNav();
  }
}

// ---------------------------------------------------------------------------
// Tracing
// ---------------------------------------------------------------------------

export interface Span {
  traceId: string;
  spanId: string;
  setAttribute(key: string, value: string | number | boolean): void;
  setMeasurement(key: string, value: number): void;
  /** Finish the span and send it. */
  end(status?: 'ok' | 'error' | 'cancelled', http?: { method?: string; status_code?: number }): void;
}

export interface SpanOptions {
  name: string;
  /** Kind of work, e.g. `function`, `ui.render`, `http.client` (default `function`). */
  op?: string;
  kind?: string;
  /** Start a new trace instead of nesting under the current one. */
  root?: boolean;
  attributes?: Record<string, string | number | boolean>;
}

let trace: { id: string; rootSpanId: string; transaction: string } | undefined;

function hex(bytes: number): string {
  let out = '';
  const c = hasWindow ? window.crypto : undefined;
  if (c && c.getRandomValues) {
    const a = new Uint8Array(bytes);
    c.getRandomValues(a);
    a.forEach((b) => (out += b.toString(16).padStart(2, '0')));
    return out;
  }
  for (let i = 0; i < bytes * 2; i++) out += ((Math.random() * 16) | 0).toString(16);
  return out;
}

/** The current trace id, if this page load is traced. */
export function traceId(): string | undefined {
  return trace?.id;
}

/**
 * Time an operation. Child of the current page load's trace unless `root` is set or the page is
 * not traced (then it starts its own trace).
 */
export function startSpan(o: SpanOptions): Span {
  const start = Date.now();
  const nested = !o.root && trace;
  const tid = nested ? trace!.id : hex(16);
  const sid = hex(8);
  const attributes: Record<string, string | number | boolean> = { ...(o.attributes || {}) };
  const measurements: Record<string, number> = {};
  let done = false;
  return {
    traceId: tid,
    spanId: sid,
    setAttribute: (k, v) => {
      attributes[k] = v;
    },
    setMeasurement: (k, v) => {
      measurements[k] = v;
    },
    end: (status = 'ok', http) => {
      if (done || !started) return;
      done = true;
      sendSpan({
        trace_id: tid,
        span_id: sid,
        parent_span_id: nested ? trace!.rootSpanId : undefined,
        name: o.name,
        op: o.op || 'function',
        kind: o.kind,
        status,
        transaction: nested ? trace!.transaction : undefined,
        start_timestamp: start,
        timestamp: Date.now(),
        http,
        attributes: Object.keys(attributes).length ? attributes : undefined,
        measurements: Object.keys(measurements).length ? measurements : undefined,
      });
    },
  };
}

function sendSpan(span: Record<string, unknown>) {
  enqueue({ type: 'span', platform: 'javascript', ...span, ...common() } as Item);
}

function propagate(url: string): boolean {
  const targets = opts.tracePropagationTargets;
  if (!targets) {
    if (!hasWindow) return false;
    return url.indexOf('/') === 0 || url.indexOf(location.origin) === 0;
  }
  return targets.some((t) => (typeof t === 'string' ? url.indexOf(t) === 0 : t.test(url)));
}

function installTracing() {
  const path = location.pathname;
  trace = { id: hex(16), rootSpanId: hex(8), transaction: path };
  const t = trace;
  const perf = window.performance;
  const origin = perf && perf.timeOrigin ? perf.timeOrigin : Date.now();
  const vitals: Record<string, number> = {};
  const sendPageload = () => {
    const nav = perf && perf.getEntriesByType ? (perf.getEntriesByType('navigation')[0] as PerformanceNavigationTiming | undefined) : undefined;
    const end = nav && nav.loadEventEnd > 0 ? nav.loadEventEnd : perf ? perf.now() : 0;
    const m: Record<string, number> = {};
    if (nav) {
      m.ttfb = Math.max(0, nav.responseStart);
      m.dom_content_loaded = nav.domContentLoadedEventEnd;
      m.load = nav.loadEventEnd;
      if (nav.responseStart > 0) vitals.ttfb = Math.max(0, nav.responseStart);
    }
    sendSpan({ trace_id: t.id, span_id: t.rootSpanId, name: path, op: 'pageload', transaction: path, start_timestamp: Math.round(origin), timestamp: Math.round(origin + end), measurements: m });
  };
  if (document.readyState === 'complete') setTimeout(sendPageload, 0);
  else window.addEventListener('load', () => setTimeout(sendPageload, 0), { once: true });
  const observe = (type: string, cb: (entries: PerformanceEntry[]) => void, extra?: Record<string, unknown>) => {
    try {
      if (typeof PerformanceObserver === 'undefined' || !(PerformanceObserver.supportedEntryTypes || []).includes(type)) return;
      new PerformanceObserver((list) => cb(list.getEntries())).observe({ type, buffered: true, ...(extra || {}) } as PerformanceObserverInit);
    } catch {
      /* unsupported */
    }
  };
  observe('paint', (es) => es.forEach((e) => e.name === 'first-contentful-paint' && (vitals.fcp = e.startTime)));
  observe('largest-contentful-paint', (es) => es.length && (vitals.lcp = es[es.length - 1].startTime));
  let cls = 0;
  observe('layout-shift', (es) =>
    es.forEach((e) => {
      const ls = e as PerformanceEntry & { value: number; hadRecentInput: boolean };
      if (!ls.hadRecentInput) vitals.cls = cls += ls.value;
    }),
  );
  observe('event', (es) => es.forEach((e) => (e as PerformanceEntry & { interactionId?: number }).interactionId && (vitals.inp = Math.max(vitals.inp || 0, e.duration))), { durationThreshold: 40 });
  let sent = false;
  const sendVitals = () => {
    if (sent || !Object.keys(vitals).length) return;
    sent = true;
    const now = Date.now();
    sendSpan({ trace_id: t.id, span_id: hex(8), parent_span_id: t.rootSpanId, name: path, op: 'web.vitals', transaction: path, start_timestamp: Math.round(origin), timestamp: now, measurements: { ...vitals } });
  };
  document.addEventListener('visibilitychange', () => document.visibilityState === 'hidden' && sendVitals());
  window.addEventListener('pagehide', sendVitals);
}

/** For tests: reset all state. */
export function _reset(): void {
  started = false;
  disabled = false;
  queue = [];
  crumbs = [];
  memSession = undefined;
  trace = undefined;
  userId = undefined;
  tags = {};
  lastUrl = '';
  backoffUntil = 0;
  if (timer) clearTimeout(timer);
  timer = undefined;
  origFetch = typeof fetch !== 'undefined' ? fetch.bind(globalThis) : origFetch;
}

export { parseStack };
