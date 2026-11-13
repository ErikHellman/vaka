import { readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export interface Frame {
  function?: string;
  module?: string;
  abs_path?: string;
  filename?: string;
  lineno?: number;
  colno?: number;
  in_app?: boolean;
  context_line?: string;
  pre_context?: string[];
  post_context?: string[];
}

// V8: "    at fn (/srv/app.js:10:5)", "    at /srv/app.js:10:5", "    at async fn (file:///srv/app.mjs:1:2)",
// "    at fn (node:internal/x:1:2)", "    at new Foo (C:\\app\\x.js:1:2)".
const V8 = /^\s*at (?:(.+?) \()?((?:[a-z][a-z0-9+.-]*:|\/|[a-z]:\\|\\\\)[^)]*?):(\d+):(\d+)\)?\s*$/i;
const V8_NOPOS = /^\s*at (?:(.+?) \()?(native|<anonymous>|node:[^)]+)\)?\s*$/i;

/** Parse a V8 (Node) stack string into frames, crash site first. */
export function parseStack(stack: string | undefined, cwd: string = safeCwd()): Frame[] {
  if (!stack) return [];
  const frames: Frame[] = [];
  for (const line of stack.split('\n').slice(0, 100)) {
    let fn: string | undefined;
    let path: string;
    let lineno: number | undefined;
    let colno: number | undefined;
    const m = V8.exec(line);
    if (m) {
      fn = m[1];
      path = m[2];
      lineno = +m[3];
      colno = +m[4];
    } else {
      const n = V8_NOPOS.exec(line);
      if (!n) continue;
      fn = n[1];
      path = n[2];
    }
    if (path.startsWith('file://')) {
      try {
        path = fileURLToPath(path);
      } catch {
        /* keep as is */
      }
    }
    if (fn) fn = fn.replace(/^async /, '').replace(/^new /, '').replace(/ \[as [^\]]+\]$/, '');
    const internal = /^node:|^native$|^<anonymous>$|^internal\//.test(path);
    const vendor = /[\\/]node_modules[\\/]/.test(path);
    frames.push({
      function: fn || '<anonymous>',
      module: moduleName(path, cwd),
      abs_path: path,
      filename: relative(path, cwd),
      lineno,
      colno,
      in_app: !internal && !vendor,
    });
  }
  return frames;
}

function safeCwd(): string {
  try {
    return process.cwd();
  } catch {
    return '';
  }
}

function relative(path: string, cwd: string): string {
  if (cwd && path.startsWith(cwd + '/')) return path.slice(cwd.length + 1);
  if (cwd && path.startsWith(cwd + '\\')) return path.slice(cwd.length + 1).replace(/\\/g, '/');
  return path;
}

/** `node_modules/@scope/pkg/lib/x.js` -> `@scope/pkg:lib/x.js`; app files -> path without extension. */
function moduleName(path: string, cwd: string): string | undefined {
  const norm = path.replace(/\\/g, '/');
  const i = norm.lastIndexOf('/node_modules/');
  if (i >= 0) {
    const rest = norm.slice(i + 14).split('/');
    const pkg = rest[0].startsWith('@') ? rest.slice(0, 2).join('/') : rest[0];
    const file = rest.slice(rest[0].startsWith('@') ? 2 : 1).join('/');
    return `${pkg}:${file.replace(/\.[cm]?[jt]sx?$/, '')}`;
  }
  if (norm.startsWith('node:')) return norm.slice(5);
  const rel = relative(path, cwd).replace(/\\/g, '/');
  return rel.startsWith('/') ? undefined : rel.replace(/\.[cm]?[jt]sx?$/, '');
}

const CONTEXT_LINES = 5;
const MAX_FILE_BYTES = 1 << 20;
const cache = new Map<string, string[] | null>();

function lines(path: string): string[] | null {
  if (cache.has(path)) return cache.get(path)!;
  let out: string[] | null = null;
  try {
    if (statSync(path).size <= MAX_FILE_BYTES) out = readFileSync(path, 'utf8').split(/\r?\n/);
  } catch {
    out = null;
  }
  if (cache.size > 200) cache.delete(cache.keys().next().value as string);
  cache.set(path, out);
  return out;
}

/** Add `context_line`, `pre_context` and `post_context` from disk to the first in-app frames. */
export function addSourceContext(frames: Frame[], maxFrames = 10): Frame[] {
  let n = 0;
  for (const f of frames) {
    if (n >= maxFrames) break;
    if (!f.in_app || !f.abs_path || !f.lineno) continue;
    const src = lines(f.abs_path);
    if (!src || f.lineno > src.length) continue;
    n++;
    const i = f.lineno - 1;
    const cut = (s: string) => (s.length > 300 ? s.slice(0, 300) + '…' : s);
    f.context_line = cut(src[i]);
    f.pre_context = src.slice(Math.max(0, i - CONTEXT_LINES), i).map(cut);
    f.post_context = src.slice(i + 1, i + 1 + CONTEXT_LINES).map(cut);
  }
  return frames;
}
