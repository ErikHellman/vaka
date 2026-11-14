/**
 * Vaka feedback widget: a floating button and dialog that sends user feedback with an optional
 * screenshot, plus a "what happened?" prompt after an uncaught error.
 *
 *   import * as Vaka from '@hellsoft/vaka-browser';
 *   import { installFeedback, showFeedback } from '@hellsoft/vaka-browser/feedback';
 *   Vaka.init({ key: 'vk_...', endpoint: 'https://vaka.example.com' });
 *   installFeedback({ button: true, crashPrompt: true });
 *   // or open it from your own menu item:
 *   showFeedback();
 *
 * Mark elements that must never appear in screenshots with `data-vaka-mask`; password fields are
 * always masked.
 */
import { captureFeedback, onUnhandledError, isActive, FeedbackResult } from './index';

export interface FeedbackStrings {
  button: string;
  title: string;
  crashTitle: string;
  crashIntro: string;
  placeholder: string;
  emailLabel: string;
  screenshotLabel: string;
  submit: string;
  cancel: string;
  sending: string;
  thanks: string;
  error: string;
}

export interface FeedbackWidgetOptions {
  /** Show a floating feedback button (default true). */
  button?: boolean;
  /** Corner for the button (default bottom-right). */
  position?: 'bottom-right' | 'bottom-left';
  /** Ask "what happened?" after an uncaught error, at most once a minute (default false). */
  crashPrompt?: boolean;
  /** Offer a screenshot of the page; the user can remove it before sending (default true). */
  screenshot?: boolean;
  /** Show the optional email field (default true). */
  askEmail?: boolean;
  /** Prefill the email field, e.g. for signed-in users. */
  email?: string;
  /** Accent color (default #2563eb). */
  accent?: string;
  zIndex?: number;
  /** Override any of the texts (default English). */
  strings?: Partial<FeedbackStrings>;
  /** Called after feedback was sent. */
  onSubmit?: (result: FeedbackResult | undefined) => void;
}

export interface ShowOptions {
  /** Error event the feedback is about. */
  eventId?: string;
  /** Prefill the message. */
  message?: string;
  /** Opened by the crash prompt (changes title and source). */
  crash?: boolean;
}

const DEFAULT_STRINGS: FeedbackStrings = {
  button: 'Feedback',
  title: 'Send feedback',
  crashTitle: 'Something went wrong',
  crashIntro: 'Sorry about that. What were you doing when it happened?',
  placeholder: "What's on your mind? Tell us what happened or what you'd like to see.",
  emailLabel: 'Email (optional, if you want a reply)',
  screenshotLabel: 'Include screenshot',
  submit: 'Send',
  cancel: 'Cancel',
  sending: 'Sending…',
  thanks: 'Thanks! Your feedback was sent.',
  error: 'Could not send your feedback. Please try again.',
};

let cfg: FeedbackWidgetOptions & { strings: FeedbackStrings } = { strings: DEFAULT_STRINGS };
let host: HTMLElement | undefined;
let root: ShadowRoot | undefined;
let open = false;
let lastPrompt = 0;
let unsubscribe: (() => void) | undefined;

const hasDom = typeof window !== 'undefined' && typeof document !== 'undefined';

/** Add the feedback button and (optionally) the crash prompt. Call after Vaka.init(). */
export function installFeedback(options: FeedbackWidgetOptions = {}): void {
  cfg = { button: true, screenshot: true, askEmail: true, position: 'bottom-right', ...options, strings: { ...DEFAULT_STRINGS, ...(options.strings || {}) } };
  if (!hasDom) return;
  const mount = () => {
    ensureHost();
    renderButton();
  };
  if (document.body) mount();
  else document.addEventListener('DOMContentLoaded', mount, { once: true });
  if (unsubscribe) unsubscribe();
  unsubscribe = cfg.crashPrompt
    ? onUnhandledError((eventId) => {
        const t = Date.now();
        if (open || t - lastPrompt < 60_000) return;
        lastPrompt = t;
        // Let the app settle (and the error batch go out) before asking.
        setTimeout(() => showFeedback({ eventId, crash: true }), 600);
      })
    : undefined;
}

/** Remove the button and dialog. */
export function uninstallFeedback(): void {
  if (unsubscribe) unsubscribe();
  unsubscribe = undefined;
  if (host && host.parentNode) host.parentNode.removeChild(host);
  host = undefined;
  root = undefined;
  open = false;
}

function ensureHost(): ShadowRoot | undefined {
  if (!hasDom || !document.body) return undefined;
  if (root && host && host.isConnected) return root;
  host = document.createElement('div');
  host.setAttribute('data-vaka-feedback', '');
  host.setAttribute('data-vaka-mask', '');
  root = host.attachShadow({ mode: 'open' });
  const style = document.createElement('style');
  style.textContent = css(cfg.accent || '#2563eb', cfg.zIndex || 2147483000);
  root.appendChild(style);
  document.body.appendChild(host);
  return root;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string> = {}, text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const k of Object.keys(attrs)) e.setAttribute(k, attrs[k]);
  if (text !== undefined) e.textContent = text;
  return e;
}

function renderButton() {
  const r = ensureHost();
  if (!r) return;
  const old = r.querySelector('.vk-btn');
  if (old) old.remove();
  if (!cfg.button) return;
  const b = el('button', { class: `vk-btn ${cfg.position === 'bottom-left' ? 'left' : 'right'}`, type: 'button', 'aria-label': cfg.strings.button }, cfg.strings.button);
  b.addEventListener('click', () => showFeedback());
  r.appendChild(b);
}

/** Open the feedback dialog (captures a screenshot first when enabled). */
export async function showFeedback(o: ShowOptions = {}): Promise<void> {
  if (!hasDom || open) return;
  if (!isActive()) {
    if (typeof console !== 'undefined') console.warn('[vaka] showFeedback: call Vaka.init() first');
    return;
  }
  const r = ensureHost();
  if (!r) return;
  open = true;
  const s = cfg.strings;
  let shot: Blob | undefined;
  if (cfg.screenshot !== false) {
    // The widget is hidden while the page is captured.
    if (host) host.style.display = 'none';
    shot = await withTimeout(captureScreenshot(), 4000);
    if (host) host.style.display = '';
  }
  const overlay = el('div', { class: 'vk-overlay' });
  const dialog = el('form', { class: 'vk-dialog', role: 'dialog', 'aria-modal': 'true', 'aria-label': o.crash ? s.crashTitle : s.title });
  dialog.appendChild(el('h2', {}, o.crash ? s.crashTitle : s.title));
  if (o.crash) dialog.appendChild(el('p', { class: 'vk-intro' }, s.crashIntro));
  const ta = el('textarea', { maxlength: '5000', rows: '5', placeholder: s.placeholder, required: '' });
  if (o.message) ta.value = o.message;
  dialog.appendChild(ta);
  let email: HTMLInputElement | undefined;
  if (cfg.askEmail !== false) {
    const label = el('label', { class: 'vk-field' }, s.emailLabel);
    email = el('input', { type: 'email', autocomplete: 'email' });
    if (cfg.email) email.value = cfg.email;
    label.appendChild(email);
    dialog.appendChild(label);
  }
  let include: HTMLInputElement | undefined;
  let url: string | undefined;
  if (shot) {
    const wrap = el('div', { class: 'vk-shot' });
    const label = el('label', {});
    include = el('input', { type: 'checkbox' });
    include.checked = true;
    label.appendChild(include);
    label.appendChild(document.createTextNode(' ' + s.screenshotLabel));
    url = URL.createObjectURL(shot);
    const img = el('img', { src: url, alt: '' });
    include.addEventListener('change', () => (img.style.opacity = include!.checked ? '1' : '0.25'));
    wrap.appendChild(label);
    wrap.appendChild(img);
    dialog.appendChild(wrap);
  }
  const status = el('p', { class: 'vk-status', role: 'status' });
  dialog.appendChild(status);
  const row = el('div', { class: 'vk-row' });
  const cancel = el('button', { type: 'button', class: 'vk-ghost' }, s.cancel);
  const submit = el('button', { type: 'submit' }, s.submit);
  row.appendChild(cancel);
  row.appendChild(submit);
  dialog.appendChild(row);
  overlay.appendChild(dialog);
  r.appendChild(overlay);
  ta.focus();

  const close = () => {
    overlay.remove();
    if (url) URL.revokeObjectURL(url);
    document.removeEventListener('keydown', onKey, true);
    open = false;
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') close();
  };
  document.addEventListener('keydown', onKey, true);
  cancel.addEventListener('click', close);
  overlay.addEventListener('click', (e) => e.target === overlay && close());
  dialog.addEventListener('submit', async (e) => {
    e.preventDefault();
    const message = ta.value.trim();
    if (!message) return;
    submit.disabled = true;
    status.textContent = s.sending;
    const res = await captureFeedback({
      message,
      email: email && email.value.trim() ? email.value.trim() : undefined,
      eventId: o.eventId,
      source: o.crash ? 'crash_prompt' : 'widget',
      attachments: shot && include && include.checked ? [shot] : undefined,
    });
    if (cfg.onSubmit) cfg.onSubmit(res);
    if (!res) {
      status.textContent = s.error;
      submit.disabled = false;
      return;
    }
    status.textContent = s.thanks;
    ta.disabled = true;
    setTimeout(close, 1500);
  });
}

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T | undefined> {
  return new Promise((resolve) => {
    const t = setTimeout(() => resolve(undefined), ms);
    p.then(
      (v) => {
        clearTimeout(t);
        resolve(v);
      },
      () => {
        clearTimeout(t);
        resolve(undefined);
      },
    );
  });
}

// ---------------------------------------------------------------------------
// Screenshot: the visible part of the page, rendered from a styled DOM clone through an SVG
// <foreignObject>. Web fonts, cross-origin images, iframes and pseudo-elements are left out.
// Browsers that refuse to export such a canvas (some Safari versions) get no screenshot.
// ---------------------------------------------------------------------------

const MAX_ELEMENTS = 6000;
const SKIP = new Set(['SCRIPT', 'NOSCRIPT', 'TEMPLATE', 'LINK', 'META', 'STYLE', 'HEAD', 'TITLE']);

function masked(e: Element): boolean {
  return e.hasAttribute('data-vaka-mask') || (e.tagName === 'INPUT' && (e as HTMLInputElement).type === 'password');
}

function styleText(cs: CSSStyleDeclaration): string {
  let out = '';
  for (let i = 0; i < cs.length; i++) {
    const name = cs[i];
    // Animations and transitions would freeze mid-way or be ignored; skip them.
    if (name.indexOf('animation') === 0 || name.indexOf('transition') === 0) continue;
    out += `${name}:${cs.getPropertyValue(name)};`;
  }
  return out;
}

function imageData(img: HTMLImageElement): string | undefined {
  try {
    if (!img.complete || !img.naturalWidth) return undefined;
    const c = document.createElement('canvas');
    c.width = Math.min(img.naturalWidth, 1200);
    c.height = Math.round((img.naturalHeight * c.width) / img.naturalWidth);
    const ctx = c.getContext('2d');
    if (!ctx) return undefined;
    ctx.drawImage(img, 0, 0, c.width, c.height);
    return c.toDataURL('image/jpeg', 0.7);
  } catch {
    return undefined; // cross-origin
  }
}

function cloneNode(n: Node, budget: { left: number }): Node | null {
  if (n.nodeType === 3) return document.createTextNode(n.nodeValue || '');
  if (n.nodeType !== 1) return null;
  const e = n as HTMLElement;
  if (SKIP.has(e.tagName) || e.hasAttribute('data-vaka-feedback')) return null;
  if (--budget.left < 0) throw new Error('page too large to capture');
  const cs = window.getComputedStyle(e);
  if (cs.display === 'none') return null;
  const isSvg = e.namespaceURI === 'http://www.w3.org/2000/svg';
  if (masked(e)) {
    const r = e.getBoundingClientRect();
    const box = document.createElement('div');
    box.setAttribute('style', `display:${cs.display === 'inline' ? 'inline-block' : cs.display};width:${r.width}px;height:${r.height}px;background:#9ca3af;border-radius:3px;margin:${cs.margin}`);
    return box;
  }
  let c: HTMLElement;
  if (e.tagName === 'IFRAME' || e.tagName === 'VIDEO' || e.tagName === 'CANVAS') {
    const r = e.getBoundingClientRect();
    c = document.createElement('div');
    let data: string | undefined;
    if (e.tagName === 'CANVAS') {
      try {
        data = (e as HTMLCanvasElement).toDataURL();
      } catch {
        data = undefined;
      }
    }
    const display = cs.display === 'inline' ? 'inline-block' : cs.display;
    c.setAttribute('style', `${styleText(cs)}display:${display};width:${r.width}px;height:${r.height}px;background:${data ? `url(${data}) center/100% 100% no-repeat` : '#e5e7eb'}`);
    return c;
  }
  c = e.cloneNode(false) as HTMLElement;
  if (!isSvg) c.setAttribute('style', styleText(cs));
  c.removeAttribute('id');
  if (e.tagName === 'IMG') {
    const data = imageData(e as HTMLImageElement);
    if (data) c.setAttribute('src', data);
    else c.removeAttribute('src');
    c.removeAttribute('srcset');
  } else if (e.tagName === 'INPUT') {
    const i = e as HTMLInputElement;
    if (i.type === 'checkbox' || i.type === 'radio') {
      if (i.checked) c.setAttribute('checked', '');
      else c.removeAttribute('checked');
    } else c.setAttribute('value', i.value);
  } else if (e.tagName === 'TEXTAREA') {
    c.textContent = (e as HTMLTextAreaElement).value;
    return c;
  } else if (e.tagName === 'SELECT') {
    const sel = e as HTMLSelectElement;
    Array.prototype.forEach.call(sel.options, (o: HTMLOptionElement, idx: number) => {
      const oc = document.createElement('option');
      oc.textContent = o.text;
      if (idx === sel.selectedIndex) oc.setAttribute('selected', '');
      c.appendChild(oc);
    });
    return c;
  }
  const kids = e.shadowRoot ? e.shadowRoot.childNodes : e.childNodes;
  for (let i = 0; i < kids.length; i++) {
    const k = cloneNode(kids[i], budget);
    if (k) c.appendChild(k);
  }
  return c;
}

/** A JPEG of the visible viewport (longest side at most `maxSide` px), or undefined. */
export async function captureScreenshot(maxSide = 1600): Promise<Blob | undefined> {
  if (!hasDom || !document.body) return undefined;
  try {
    const w = window.innerWidth;
    const h = window.innerHeight;
    const body = cloneNode(document.body, { left: MAX_ELEMENTS }) as HTMLElement | null;
    if (!body) return undefined;
    const htmlBg = window.getComputedStyle(document.documentElement).backgroundColor;
    const bodyBg = window.getComputedStyle(document.body).backgroundColor;
    const bg = bodyBg && bodyBg !== 'rgba(0, 0, 0, 0)' ? bodyBg : htmlBg && htmlBg !== 'rgba(0, 0, 0, 0)' ? htmlBg : '#ffffff';
    const NS = 'http://www.w3.org/1999/xhtml';
    const frame = document.createElementNS(NS, 'div') as HTMLElement;
    frame.setAttribute('style', `width:${w}px;height:${h}px;overflow:hidden;background:${bg};position:relative`);
    const inner = document.createElementNS(NS, 'div') as HTMLElement;
    inner.setAttribute('style', `position:absolute;left:0;top:0;width:${document.documentElement.scrollWidth}px;transform:translate(${-window.scrollX}px,${-window.scrollY}px)`);
    body.style.margin = window.getComputedStyle(document.body).margin;
    inner.appendChild(body);
    frame.appendChild(inner);
    const xhtml = new XMLSerializer().serializeToString(frame);
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}"><foreignObject x="0" y="0" width="100%" height="100%">${xhtml}</foreignObject></svg>`;
    const img = new Image();
    img.decoding = 'async';
    await new Promise<void>((resolve, reject) => {
      img.onload = () => resolve();
      img.onerror = () => reject(new Error('render failed'));
      img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
    });
    // Up to 2x for sharp text on high-DPI screens, never more than maxSide on the long side.
    const factor = Math.min(maxSide / Math.max(w, h), Math.min(window.devicePixelRatio || 1, 2));
    const canvas = document.createElement('canvas');
    canvas.width = Math.round(w * factor);
    canvas.height = Math.round(h * factor);
    const ctx = canvas.getContext('2d');
    if (!ctx) return undefined;
    ctx.fillStyle = bg;
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
    return await new Promise<Blob | undefined>((resolve) => {
      try {
        canvas.toBlob((b) => resolve(b || undefined), 'image/jpeg', 0.8);
      } catch {
        resolve(undefined); // tainted canvas
      }
    });
  } catch {
    return undefined;
  }
}

function css(accent: string, z: number): string {
  return `
:host{all:initial}
*{box-sizing:border-box;font-family:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
.vk-btn{position:fixed;bottom:20px;z-index:${z};padding:10px 16px;border:0;border-radius:999px;background:${accent};color:#fff;font-size:14px;font-weight:600;cursor:pointer;box-shadow:0 4px 14px rgba(0,0,0,.2)}
.vk-btn.right{right:20px}.vk-btn.left{left:20px}
.vk-overlay{position:fixed;inset:0;z-index:${z + 1};background:rgba(0,0,0,.4);display:flex;align-items:center;justify-content:center;padding:16px}
.vk-dialog{background:#fff;color:#1c1917;width:100%;max-width:440px;max-height:calc(100vh - 32px);overflow:auto;border-radius:12px;padding:20px;box-shadow:0 10px 40px rgba(0,0,0,.3);font-size:14px;line-height:1.45}
.vk-dialog h2{margin:0 0 8px;font-size:18px}
.vk-intro{margin:0 0 10px;color:#57534e}
textarea,input[type=email]{width:100%;font:inherit;padding:8px 10px;border:1px solid #d6d3d1;border-radius:8px;background:#fff;color:inherit}
textarea{resize:vertical;min-height:96px}
textarea:focus,input:focus{outline:2px solid ${accent};outline-offset:1px}
.vk-field{display:block;margin-top:10px;color:#57534e;font-size:13px}.vk-field input{margin-top:4px}
.vk-shot{margin-top:10px;font-size:13px;color:#57534e}
.vk-shot img{display:block;margin-top:6px;max-width:100%;max-height:160px;border:1px solid #e7e5e4;border-radius:6px;transition:opacity .15s}
.vk-status{min-height:1.2em;margin:10px 0 0;font-size:13px;color:#57534e}
.vk-row{display:flex;justify-content:flex-end;gap:8px;margin-top:8px}
.vk-row button{font:inherit;font-weight:600;padding:8px 16px;border-radius:8px;border:1px solid ${accent};background:${accent};color:#fff;cursor:pointer}
.vk-row button.vk-ghost{background:transparent;color:${accent}}
.vk-row button:disabled{opacity:.6;cursor:default}
@media (prefers-color-scheme:dark){.vk-dialog{background:#1c1917;color:#e7e5e4}.vk-intro,.vk-field,.vk-shot,.vk-status{color:#a8a29e}textarea,input[type=email]{background:#0c0a09;border-color:#44403c}}
`;
}
