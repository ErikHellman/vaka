export interface Frame {
  function?: string;
  abs_path?: string;
  filename?: string;
  lineno?: number;
  colno?: number;
  in_app?: boolean;
}

// Chrome / V8 / Edge:   "    at fn (https://x/app.js:10:5)" or "    at https://x/app.js:10:5"
const CHROME = /^\s*at (?:(.+?) \()?((?:[a-z-]+:\/\/|\/|[a-z]:\\|<anonymous>|native|eval)[^)]*?):(\d+):(\d+)\)?\s*$/i;
// Firefox / Safari:     "fn@https://x/app.js:10:5" or "@https://x/app.js:10:5"
const GECKO = /^\s*(.*?)(?:\((.*?)\))?(?:^|@)((?:[a-z-]+:\/\/|\/|[a-z]:\\)[^@]*?|\[native code\]):(\d+)(?::(\d+))?\s*$/i;

function fileName(url: string): string {
  const clean = url.split(/[?#]/)[0];
  const parts = clean.split('/');
  return parts[parts.length - 1] || clean;
}

/** Parse a JS stack string into frames, crash site first. */
export function parseStack(stack: string | undefined, origin?: string): Frame[] {
  if (!stack) return [];
  const frames: Frame[] = [];
  for (const line of stack.split('\n').slice(0, 60)) {
    let m = CHROME.exec(line);
    let fn: string | undefined;
    let url: string;
    let lineno: number;
    let colno: number | undefined;
    if (m) {
      fn = m[1];
      url = m[2];
      lineno = +m[3];
      colno = +m[4];
    } else if ((m = GECKO.exec(line))) {
      fn = m[1] || undefined;
      url = m[3];
      lineno = +m[4];
      colno = m[5] ? +m[5] : undefined;
    } else {
      continue;
    }
    if (fn) fn = fn.replace(/^async /, '').replace(/^new /, '');
    const inApp =
      !/node_modules|chrome-extension:|moz-extension:|safari-extension:|<anonymous>|\[native code\]/.test(url) &&
      (!origin || !/^[a-z-]+:\/\//i.test(url) || url.indexOf(origin) === 0);
    frames.push({ function: fn || '?', abs_path: url, filename: fileName(url), lineno, colno, in_app: inApp });
  }
  return frames;
}
