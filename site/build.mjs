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
  ['Performance', 'performance'],
  ['Feature flags', 'flags'],
  ['Anomalies and digests', 'anomalies'],
  ['OpenTelemetry', 'opentelemetry'],
  ['Privacy', 'privacy'],
  ['Ingest protocol', 'ingest'],
  ['SDK: JavaScript', 'sdk-javascript'],
  ['SDK: Android', 'sdk-android'],
  ['SDK: iOS', 'sdk-ios'],
  ['SDK: Rust', 'sdk-rust'],
  ['SDK: Go', 'sdk-go'],
  ['SDK: .NET', 'sdk-dotnet'],
  ['SDK: HTTP', 'sdk-http'],
];
const installs = ['javascript', 'android', 'ios', 'rust', 'go', 'dotnet', 'http'];
const installLabel = { javascript: 'JavaScript', android: 'Android', ios: 'iOS', rust: 'Rust', go: 'Go', dotnet: '.NET', http: 'Other (HTTP)' };

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
<title>${title} · Vaka</title>
<link rel="alternate" type="text/markdown" href="${up}${current}.md">
<style>${css}</style>
</head>
<body>
<header><a class="brand" href="${up || './'}">vaka</a><a href="https://github.com/ErikHellman/vaka">GitHub</a></header>
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
fs.writeFileSync(path.join(out, 'CNAME'), 'vaka.hellsoft.se\n');
fs.writeFileSync(path.join(out, '.nojekyll'), '');
console.log('built site/dist');
