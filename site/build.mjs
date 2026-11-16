// Builds the static docs site for GitHub Pages (vaka.hellsoft.se) from ../docs.
// Output: site/dist. Placeholders like {{ENDPOINT}} become example values.
import { marked } from 'marked';
import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '..');
const docs = path.join(root, 'docs');
const out = path.join(import.meta.dirname, 'dist');
const ENDPOINT = 'https://vaka.example.com';

const nav = [
  ['Overview', 'index'],
  ['Quickstart', 'quickstart'],
  ['Self-hosting', 'self-hosting'],
  ['Deploy on Fly.io', 'deploy-fly'],
  ['MCP server', 'mcp'],
  ['REST API', 'api'],
  ['CLI', 'cli'],
  ['Alerts', 'alerts'],
  ['Monitoring', 'monitoring'],
  ['Real-time', 'realtime'],
  ['Grafana', 'grafana'],
  ['Performance', 'performance'],
  ['Feature flags', 'flags'],
  ['Anomalies and digests', 'anomalies'],
  ['User feedback', 'feedback'],
  ['OpenTelemetry', 'opentelemetry'],
  ['Privacy', 'privacy'],
  ['Ingest protocol', 'ingest'],
  ['SDK: JavaScript', 'sdk-javascript'],
  ['SDK: Android', 'sdk-android'],
  ['SDK: iOS', 'sdk-ios'],
  ['SDK: Rust', 'sdk-rust'],
  ['SDK: Go', 'sdk-go'],
  ['SDK: JVM', 'sdk-jvm'],
  ['SDK: Node.js', 'sdk-node'],
  ['SDK: Python', 'sdk-python'],
  ['SDK: .NET', 'sdk-dotnet'],
  ['SDK: HTTP', 'sdk-http'],
];
const installs = ['javascript', 'android', 'ios', 'rust', 'go', 'jvm', 'node', 'python', 'dotnet', 'http'];
const installLabel = { javascript: 'JavaScript', android: 'Android', ios: 'iOS', rust: 'Rust', go: 'Go', jvm: 'JVM', node: 'Node.js', python: 'Python', dotnet: '.NET', http: 'Other (HTTP)' };

const fill = (s) =>
  s.replaceAll('{{ENDPOINT}}', ENDPOINT).replaceAll('{{KEY}}', 'vk_your_ingest_key').replaceAll('{{PROJECT}}', 'your-project');

// Links between docs: `api.md#x` -> `api.html#x`; instance links to /docs/... -> site pages.
const rewriteLinks = (html, depth) => {
  const up = depth ? '../'.repeat(depth) : '';
  return html
    .replaceAll(`href="${ENDPOINT}/docs/install/`, `href="${up}install/`)
    .replaceAll(`href="${ENDPOINT}/docs/`, `href="${up}`)
    .replaceAll(`href="${ENDPOINT}/llms.txt"`, `href="${up}llms.txt"`)
    .replace(/href="((?:\.\.\/)*[a-z0-9/-]+)\.md(#[^"]*)?"/g, (_, p, h = '') => `href="${p}.html${h}"`)
    .replace(/href="((?:\.\.\/)*install\/[a-z]+)"/g, (_, p) => `href="${p}.html"`)
    .replace(/href="((?:\.\.\/)*)index\.html"/g, (_, p) => `href="${p || './'}"`);
};

// Live status badges, rendered by GitHub/shields.io at view time so they never go stale.
const REPO = 'https://github.com/ErikHellman/vaka';
const badges =
  `<a href="${REPO}/actions/workflows/ci.yml?query=branch%3Amain"><img src="${REPO}/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI status" height="20"></a>` +
  `<a href="${REPO}/releases/latest"><img src="https://img.shields.io/github/v/release/ErikHellman/vaka?filter=v*&label=release&color=2f6f4f" alt="Latest release" height="20"></a>`;

const SITE = 'https://vaka.hellsoft.se';
const DESCRIPTION =
  'Vaka is a self-hosted crash reporting and analytics service built for coding agents: errors, crashes, web and product analytics for web, mobile and backend apps, queried over MCP and a REST API.';
const esc = (s) => s.replaceAll('&', '&amp;').replaceAll('"', '&quot;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');

// Favicons and social-preview tags. Absolute URLs for og:image so link unfurlers can fetch it.
function headMeta(title, current, up) {
  const url = current === 'index' ? `${SITE}/` : `${SITE}/${current}.html`;
  const full = current === 'index' ? 'Vaka: crash reporting and analytics for the agentic era' : `${title} · Vaka`;
  return `<meta name="description" content="${esc(DESCRIPTION)}">
<meta name="theme-color" content="#2f6f4f">
<link rel="canonical" href="${url}">
<link rel="icon" href="${up}favicon.ico" sizes="32x32">
<link rel="icon" href="${up}favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="${up}apple-touch-icon.png">
<meta property="og:type" content="website">
<meta property="og:site_name" content="Vaka">
<meta property="og:title" content="${esc(full)}">
<meta property="og:description" content="${esc(DESCRIPTION)}">
<meta property="og:url" content="${url}">
<meta property="og:image" content="${SITE}/og-image.png">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta property="og:image:alt" content="Vaka logo: an eye with a pulse line. Crash reporting and analytics for the agentic era.">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="${esc(full)}">
<meta name="twitter:description" content="${esc(DESCRIPTION)}">
<meta name="twitter:image" content="${SITE}/og-image.png">`;
}

const css = fs.readFileSync(path.join(import.meta.dirname, 'style.css'), 'utf8');

function page(title, body, current, depth) {
  const up = depth ? '../'.repeat(depth) : '';
  const links = nav
    .map(([label, slug]) => {
      const href = slug === 'index' ? up || './' : `${up}${slug}.html`;
      return `<a href="${href}"${slug === current ? ' aria-current="page"' : ''}>${label}</a>`;
    })
    .join('\n');
  const installLinks = installs
    .map((p) => `<a href="${up}install/${p}.html"${current === `install/${p}` ? ' aria-current="page"' : ''}>${installLabel[p]}</a>`)
    .join('\n');
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${current === 'index' ? 'Vaka: crash reporting and analytics for the agentic era' : `${title} · Vaka`}</title>
${headMeta(title, current, up)}
<link rel="alternate" type="text/markdown" href="${up}${current}.md">
<style>${css}</style>
</head>
<body>
<header><a class="brand" href="${up || './'}"><img src="${up}favicon.svg" alt="" width="24" height="24">vaka</a><div class="status">${badges}<a href="https://github.com/ErikHellman/vaka">GitHub</a></div></header>
<div class="layout">
<nav><div class="group">Docs</div>${links}<div class="group">Install guides</div>${installLinks}<div class="group">For agents</div><a href="${up}llms.txt">llms.txt</a></nav>
<main>${body}</main>
</div>
</body>
</html>
`;
}

fs.rmSync(out, { recursive: true, force: true });
fs.mkdirSync(path.join(out, 'install'), { recursive: true });

function render(src, slug, depth) {
  const md = fill(fs.readFileSync(src, 'utf8'));
  const title = (md.match(/^# (.+)$/m) || [, 'Vaka'])[1];
  const html = rewriteLinks(marked.parse(md), depth);
  const file = slug === 'index' ? 'index' : slug;
  fs.writeFileSync(path.join(out, `${file}.html`), page(title, html, slug, depth));
  fs.writeFileSync(path.join(out, `${file}.md`), md); // raw Markdown for agents
}

for (const [, slug] of nav) render(path.join(docs, `${slug}.md`), slug, 0);
for (const p of installs) render(path.join(docs, 'install', `${p}.md`), `install/${p}`, 1);

fs.writeFileSync(
  path.join(out, 'llms.txt'),
  fill(fs.readFileSync(path.join(docs, 'llms.txt'), 'utf8')).replaceAll(`${ENDPOINT}/docs/`, 'https://vaka.hellsoft.se/'),
);
// Static files (favicons, social preview image) copied as-is.
fs.cpSync(path.join(import.meta.dirname, 'public'), out, { recursive: true });
fs.writeFileSync(path.join(out, 'CNAME'), 'vaka.hellsoft.se\n');
fs.writeFileSync(path.join(out, '.nojekyll'), '');
console.log('built site/dist');
