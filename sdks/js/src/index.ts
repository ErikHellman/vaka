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
  /** Record breadcrumbs for clicks, navigation, console and failed fetches (default true). */
  breadcrumbs?: boolean;
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
  type: 'error' | 'event' | 'session';
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

function common() {
  return { release: opts.release, environment: opts.environment, user_id: userId };
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
      return f.call(window, input, init).then(
        (r) => {
          if (r.status >= 400 && url.indexOf(opts.endpoint) !== 0) addBreadcrumb({ category: 'fetch', level: 'warning', message: `${method} ${url} ${r.status}` });
          return r;
        },
        (e: unknown) => {
          if (url.indexOf(opts.endpoint) !== 0) addBreadcrumb({ category: 'fetch', level: 'error', message: `${method} ${url} failed` });
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

/** For tests: reset all state. */
export function _reset(): void {
  started = false;
  disabled = false;
  queue = [];
  crumbs = [];
  userId = undefined;
  tags = {};
  lastUrl = '';
  backoffUntil = 0;
  if (timer) clearTimeout(timer);
  timer = undefined;
  origFetch = typeof fetch !== 'undefined' ? fetch.bind(globalThis) : origFetch;
}

export { parseStack };
