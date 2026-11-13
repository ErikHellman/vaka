import { AsyncLocalStorage } from 'node:async_hooks';

export type Level = 'fatal' | 'error' | 'warning' | 'info';

export interface Breadcrumb {
  timestamp?: string;
  category?: string;
  type?: string;
  message?: string;
  level?: Level;
  data?: Record<string, unknown>;
}

export interface User {
  id: string;
  [k: string]: unknown;
}

export interface RequestInfo {
  method?: string;
  url?: string;
  route?: string;
  query_string?: string;
  headers?: Record<string, string>;
}

export const MAX_BREADCRUMBS = 100;

/** Per-request (or global) state attached to everything captured while it is active. */
export class Scope {
  user?: User;
  tags: Record<string, string> = {};
  contexts: Record<string, Record<string, unknown>> = {};
  breadcrumbs: Breadcrumb[] = [];
  flags: Record<string, string> = {};
  request?: RequestInfo;
  transaction?: string;

  clone(): Scope {
    const s = new Scope();
    s.user = this.user;
    s.tags = { ...this.tags };
    s.contexts = { ...this.contexts };
    s.breadcrumbs = this.breadcrumbs.slice();
    s.flags = { ...this.flags };
    s.request = this.request;
    s.transaction = this.transaction;
    return s;
  }

  addBreadcrumb(b: Breadcrumb, max = MAX_BREADCRUMBS): void {
    this.breadcrumbs.push({ timestamp: new Date().toISOString(), level: 'info', ...b });
    if (this.breadcrumbs.length > max) this.breadcrumbs.splice(0, this.breadcrumbs.length - max);
  }
}

export const storage = new AsyncLocalStorage<Scope>();
export let globalScope = new Scope();

/** The scope of the current request, or the global scope outside one. */
export function currentScope(): Scope {
  return storage.getStore() || globalScope;
}

/**
 * Run `fn` with a fresh scope that starts as a copy of the current one. Tags, user, breadcrumbs
 * and flags set inside stay inside (per request when used in middleware).
 */
export function withScope<T>(fn: (scope: Scope) => T): T {
  const scope = currentScope().clone();
  return storage.run(scope, () => fn(scope));
}

export function _resetScopes(): void {
  globalScope = new Scope();
  storage.disable();
}
