import { gzipSync } from 'node:zlib';
import { context } from '@opentelemetry/api';
import { suppressTracing } from '@opentelemetry/core';

export interface Item {
  type: 'error' | 'event' | 'session' | 'span';
  [k: string]: unknown;
}

export interface TransportOptions {
  endpoint: string;
  key: string;
  sdk: { name: string; version: string };
  flushIntervalMs: number;
  log: (...a: unknown[]) => void;
}

const MAX_BATCH = 1000;
const MAX_QUEUE = 5000;

/** Batches items to `POST /ingest/v1/batch`, with backoff on 429/5xx and shutdown on 401. */
export class Transport {
  private queue: Item[] = [];
  private timer: NodeJS.Timeout | undefined;
  private backoffUntil = 0;
  private backoffMs = 0;
  private inflight = new Set<Promise<void>>();
  disabled = false;

  constructor(private o: TransportOptions) {}

  send(item: Item, urgent = false): void {
    if (this.disabled) return;
    this.queue.push(item);
    if (this.queue.length > MAX_QUEUE) this.queue.splice(0, this.queue.length - MAX_QUEUE);
    if (urgent || this.queue.length >= 100) this.schedule(urgent ? 0 : 10);
    else this.schedule(this.o.flushIntervalMs);
  }

  private schedule(ms: number) {
    if (this.timer) {
      if (ms > 0) return;
      clearTimeout(this.timer);
    }
    this.timer = setTimeout(() => {
      this.timer = undefined;
      void this.drain();
    }, ms);
    this.timer.unref?.();
  }

  private async drain(): Promise<void> {
    while (this.queue.length && !this.disabled) {
      if (Date.now() < this.backoffUntil) {
        this.schedule(this.backoffUntil - Date.now());
        return;
      }
      const items = this.queue.splice(0, MAX_BATCH);
      const p = this.post(items);
      this.inflight.add(p);
      try {
        await p;
      } finally {
        this.inflight.delete(p);
      }
    }
  }

  private async post(items: Item[]): Promise<void> {
    const json = JSON.stringify({ sdk: this.o.sdk, items });
    const headers: Record<string, string> = { 'Content-Type': 'application/json', 'X-Vaka-Key': this.o.key, 'X-Vaka-Sdk': `${this.o.sdk.name}/${this.o.sdk.version}` };
    let body: string | Uint8Array = json;
    if (json.length > 2048) {
      body = gzipSync(json);
      headers['Content-Encoding'] = 'gzip';
    }
    try {
      // Never trace or breadcrumb the SDK's own requests.
      const r = await context.with(suppressTracing(context.active()), () =>
        fetch(`${this.o.endpoint}/ingest/v1/batch`, { method: 'POST', headers, body, signal: AbortSignal.timeout(15000) }),
      );
      if (r.status === 429 || r.status >= 500) return this.retry(items, r.status);
      this.backoffMs = 0;
      if (r.status === 401) {
        this.disabled = true;
        this.queue = [];
        this.o.log('ingest key rejected (401); disabling until restart');
      } else if (r.status === 413 && items.length > 1) {
        const half = Math.ceil(items.length / 2);
        await this.post(items.slice(0, half));
        await this.post(items.slice(half));
      } else if (r.status >= 400) {
        this.o.log('batch dropped', r.status, await r.text().catch(() => ''));
      } else {
        this.o.log('sent', items.length, 'items', r.status);
      }
    } catch (e) {
      this.retry(items, String((e as Error)?.message || e));
    }
  }

  private retry(items: Item[], why: unknown) {
    this.backoffMs = Math.min(300000, this.backoffMs ? this.backoffMs * 2 : 2000);
    this.backoffUntil = Date.now() + this.backoffMs;
    this.queue = items.concat(this.queue).slice(0, MAX_QUEUE);
    this.o.log('send failed, retrying in', this.backoffMs, 'ms:', why);
    this.schedule(this.backoffMs);
  }

  /** Send everything queued now, waiting at most `timeoutMs`. Resolves true when all was sent. */
  async flush(timeoutMs = 5000): Promise<boolean> {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    this.backoffUntil = 0;
    const work = (async () => {
      await Promise.all([...this.inflight]);
      await this.drain();
      return this.queue.length === 0;
    })();
    let t: NodeJS.Timeout | undefined;
    const timeout = new Promise<boolean>((res) => {
      t = setTimeout(() => res(false), timeoutMs);
      t.unref?.();
    });
    try {
      return await Promise.race([work, timeout]);
    } finally {
      if (t) clearTimeout(t);
    }
  }

  pending(): number {
    return this.queue.length;
  }
}
