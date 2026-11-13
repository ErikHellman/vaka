import diagnostics from 'node:diagnostics_channel';
import { format } from 'node:util';
import { currentScope, storage, Scope, type Level, type RequestInfo } from './scope';

type Crumb = Parameters<Scope['addBreadcrumb']>[0];

let addCrumb: (b: Crumb) => void = () => undefined;
let ownEndpoint = '';
const unhooks: (() => void)[] = [];

/** Breadcrumbs for console.warn/error and outgoing HTTP (fetch/undici and node:http). */
export function installBreadcrumbs(endpoint: string, add: (b: Crumb) => void): void {
  addCrumb = add;
  ownEndpoint = endpoint;
  for (const level of ['warn', 'error'] as const) {
    const orig = console[level];
    const wrapped = function (this: Console, ...args: unknown[]) {
      try {
        addCrumb({ category: 'console', level: level === 'warn' ? 'warning' : 'error', message: format(...args).slice(0, 500) });
      } catch {
        /* never break logging */
      }
      return orig.apply(this, args);
    };
    console[level] = wrapped;
    unhooks.push(() => {
      if (console[level] === wrapped) console[level] = orig;
    });
  }

  // fetch() (undici). Published synchronously in the caller's async context, so the crumb lands
  // on the right request scope.
  const starts = new WeakMap<object, { t: number; scope: Scope }>();
  sub('undici:request:create', (m: { request: { origin?: string; path?: string; method?: string } }) => {
    starts.set(m.request, { t: Date.now(), scope: currentScope() });
  });
  sub('undici:request:headers', (m: { request: { origin?: string; path?: string; method?: string }; response: { statusCode: number } }) => {
    const s = starts.get(m.request);
    httpCrumb(s?.scope, m.request.method || 'GET', `${m.request.origin || ''}${m.request.path || ''}`, m.response.statusCode, s?.t);
  });
  sub('undici:request:error', (m: { request: { origin?: string; path?: string; method?: string }; error: Error }) => {
    const s = starts.get(m.request);
    httpCrumb(s?.scope, m.request.method || 'GET', `${m.request.origin || ''}${m.request.path || ''}`, undefined, s?.t, m.error);
  });

  // node:http / node:https clients (Node 20+ publishes these).
  type Req = { method?: string; protocol?: string; host?: string; path?: string };
  const httpStarts = new WeakMap<object, { t: number; scope: Scope }>();
  sub('http.client.request.start', (m: { request: Req }) => httpStarts.set(m.request, { t: Date.now(), scope: currentScope() }));
  sub('http.client.response.finish', (m: { request: Req; response: { statusCode?: number } }) => {
    const s = httpStarts.get(m.request);
    httpCrumb(s?.scope, m.request.method || 'GET', `${m.request.protocol || 'http:'}//${m.request.host || ''}${m.request.path || ''}`, m.response.statusCode, s?.t);
  });
}

function sub(name: string, fn: (m: any) => void) {
  const safe = (m: unknown) => {
    try {
      fn(m);
    } catch {
      /* ignore */
    }
  };
  diagnostics.subscribe(name, safe);
  unhooks.push(() => diagnostics.unsubscribe(name, safe));
}

function stripQuery(url: string): string {
  const i = url.search(/[?#]/);
  return i >= 0 ? url.slice(0, i) : url;
}

function httpCrumb(scope: Scope | undefined, method: string, url: string, status?: number, t?: number, err?: Error) {
  if (ownEndpoint && url.startsWith(ownEndpoint)) return;
  const clean = stripQuery(url);
  const level: Level = err ? 'error' : status && status >= 500 ? 'error' : status && status >= 400 ? 'warning' : 'info';
  const b: Crumb = {
    category: 'http',
    level,
    message: err ? `${method} ${clean} failed: ${err.message}` : `${method} ${clean} ${status}`,
    data: { method, url: clean, status_code: status, duration_ms: t ? Date.now() - t : undefined },
  };
  if (scope) scope.addBreadcrumb(b);
  else addCrumb(b);
}

export function uninstallIntegrations(): void {
  while (unhooks.length) unhooks.pop()!();
}

// ---------------------------------------------------------------------------
// Process-level crash capture
// ---------------------------------------------------------------------------

export interface ProcessHooks {
  capture: (err: unknown, fatal: boolean, mechanism: string) => void;
  flush: (ms: number) => Promise<unknown>;
  /** 'strict' exits like Node's default; 'warn' logs and keeps running; 'none' only reports. */
  unhandledRejection: 'strict' | 'warn' | 'none';
  exitOnUncaught: boolean;
  shutdownTimeoutMs: number;
}

export function installProcessHandlers(h: ProcessHooks): void {
  const onUncaught = (err: unknown, origin: string) => {
    h.capture(err, true, origin === 'unhandledRejection' ? 'unhandledRejection' : 'uncaughtException');
    // Other handlers own the process: report only.
    if (!h.exitOnUncaught || process.listenerCount('uncaughtException') > 1) return;
    process.stderr.write(`${(err as Error)?.stack || String(err)}\n`);
    process.exitCode = 1;
    h.flush(h.shutdownTimeoutMs).finally(() => process.exit(1));
  };
  const onRejection = (reason: unknown) => {
    const fatal = h.unhandledRejection === 'strict' && process.listenerCount('unhandledRejection') <= 1;
    h.capture(reason, fatal, 'unhandledRejection');
    if (process.listenerCount('unhandledRejection') > 1 || h.unhandledRejection === 'none') return;
    const text = (reason as Error)?.stack || String(reason);
    if (h.unhandledRejection === 'warn') {
      process.emitWarning(`Unhandled promise rejection: ${text}`);
      return;
    }
    process.stderr.write(`${text}\n`);
    process.exitCode = 1;
    h.flush(h.shutdownTimeoutMs).finally(() => process.exit(1));
  };
  process.on('uncaughtException', onUncaught);
  process.on('unhandledRejection', onRejection);
  unhooks.push(() => {
    process.off('uncaughtException', onUncaught);
    process.off('unhandledRejection', onRejection);
  });
}

/** Node's own --unhandled-rejections setting, mapped to ours. */
export function nodeRejectionMode(): 'strict' | 'warn' | 'none' {
  const args = [...process.execArgv, ...(process.env.NODE_OPTIONS || '').split(/\s+/)];
  const flag = args.find((a) => a.startsWith('--unhandled-rejections='));
  const v = flag ? flag.split('=')[1] : 'throw';
  if (v === 'none') return 'none';
  if (v === 'warn' || v === 'warn-with-error-code') return 'warn';
  return 'strict';
}

// ---------------------------------------------------------------------------
// Framework middleware
// ---------------------------------------------------------------------------

const SAFE_HEADERS = ['user-agent', 'content-type', 'content-length', 'accept', 'referer', 'x-request-id', 'traceparent'];

export interface ReqLike {
  method?: string;
  url?: string;
  originalUrl?: string;
  headers?: Record<string, string | string[] | undefined>;
  route?: { path?: string };
  routeOptions?: { url?: string };
  routerPath?: string;
  protocol?: string;
}

export function requestInfo(req: ReqLike): RequestInfo {
  const raw = req.originalUrl || req.url || '';
  const q = raw.indexOf('?');
  const path = q >= 0 ? raw.slice(0, q) : raw;
  const headers: Record<string, string> = {};
  for (const h of SAFE_HEADERS) {
    const v = req.headers?.[h];
    if (v) headers[h] = Array.isArray(v) ? v.join(', ') : v;
  }
  const host = req.headers?.['x-forwarded-host'] || req.headers?.host;
  const proto = req.protocol || (req.headers?.['x-forwarded-proto'] as string) || 'http';
  return {
    method: req.method,
    url: host ? `${proto}://${host}${path}` : path,
    route: routeOf(req),
    query_string: q >= 0 ? raw.slice(q + 1, q + 1 + 500) : undefined,
    headers,
  };
}

function routeOf(req: ReqLike): string | undefined {
  return req.routeOptions?.url || req.routerPath || req.route?.path;
}

type Capture = (err: unknown, ctx: { handled: boolean; mechanism: string; status?: number }) => void;

/** Express/Connect middleware: one scope per request. Mount it before your routes. */
export function makeRequestHandler() {
  return function vakaRequestHandler(req: ReqLike, _res: unknown, next: (err?: unknown) => void) {
    const scope = currentScope().clone();
    scope.breadcrumbs = [];
    scope.request = requestInfo(req);
    storage.run(scope, () => next());
  };
}

/** Express error middleware. Mount it after your routes and before other error handlers. */
export function makeErrorHandler(capture: Capture, shouldCapture: (status: number) => boolean) {
  return function vakaErrorHandler(err: unknown, req: ReqLike, res: { statusCode?: number }, next: (err?: unknown) => void) {
    const e = err as { status?: number; statusCode?: number } | undefined;
    const status = e?.status || e?.statusCode || (res?.statusCode && res.statusCode >= 400 ? res.statusCode : 500);
    if (shouldCapture(status)) {
      const scope = currentScope();
      scope.request = { ...requestInfo(req), ...(scope.request?.route ? { route: scope.request.route } : {}) };
      if (!scope.request.route) scope.request.route = routeOf(req);
      capture(err, { handled: false, mechanism: 'express', status });
    }
    next(err);
  };
}

/** Fastify plugin: a scope per request and capture of errors from handlers and hooks. */
export function makeFastifyPlugin(capture: Capture, shouldCapture: (status: number) => boolean) {
  const plugin = function vakaFastify(fastify: any, _opts: unknown, done: () => void) {
    fastify.addHook('onRequest', (req: ReqLike, _reply: unknown, next: () => void) => {
      const scope = currentScope().clone();
      scope.breadcrumbs = [];
      scope.request = requestInfo(req);
      storage.run(scope, next);
    });
    fastify.addHook('onError', (req: ReqLike, _reply: unknown, err: { statusCode?: number }, next: () => void) => {
      const status = err?.statusCode || 500;
      if (shouldCapture(status)) {
        const scope = currentScope();
        scope.request = requestInfo(req);
        capture(err, { handled: false, mechanism: 'fastify', status });
      }
      next();
    });
    done();
  } as any;
  plugin[Symbol.for('skip-override')] = true;
  plugin[Symbol.for('fastify.display-name')] = 'vaka';
  return plugin;
}
