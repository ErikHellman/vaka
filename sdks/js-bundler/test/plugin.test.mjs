import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { vakaVite, VakaWebpackPlugin, uploadSourcemaps, artifactName } from '../index.mjs';

function server() {
  const got = [];
  const srv = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      got.push({ url: req.url, auth: req.headers.authorization, body });
      res.writeHead(body.includes('broken') ? 400 : 201, { 'content-type': 'application/json' });
      res.end('{"ok":true}');
    });
  });
  return new Promise((r) => srv.listen(0, '127.0.0.1', () => r({ srv, got, url: `http://127.0.0.1:${srv.address().port}` })));
}

function build() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'vaka-plugin-'));
  fs.mkdirSync(path.join(dir, 'assets'));
  fs.writeFileSync(path.join(dir, 'assets', 'app-123.js'), 'x');
  fs.writeFileSync(path.join(dir, 'assets', 'app-123.js.map'), '{"version":3,"sources":[],"mappings":""}');
  fs.writeFileSync(path.join(dir, 'index.html'), '');
  return dir;
}

const base = { token: 'vt_x', project: 'web', release: '1.2.3', silent: true, failOnError: true };

test('artifact names follow the served URL path', () => {
  assert.equal(artifactName('/', 'assets/app.js.map'), '/assets/app.js');
  assert.equal(artifactName('/static/', 'app.js.map'), '/static/app.js');
  assert.equal(artifactName('https://cdn.example.com/s/', 'app.js.map'), '/s/app.js');
  assert.equal(artifactName(undefined, 'a.js.map'), '/a.js');
});

test('uploadSourcemaps posts each map and can delete them', async () => {
  const { srv, got, url } = await server();
  const dir = build();
  const r = await uploadSourcemaps(dir, { ...base, url, deleteAfterUpload: true });
  srv.close();
  assert.equal(r.uploaded, 1);
  assert.equal(got[0].url, '/api/v1/projects/web/artifacts');
  assert.equal(got[0].auth, 'Bearer vt_x');
  assert.match(got[0].body, /name="name"\r\n\r\n\/assets\/app-123\.js\r\n/);
  assert.match(got[0].body, /name="release"\r\n\r\n1\.2\.3\r\n/);
  assert.ok(!fs.existsSync(path.join(dir, 'assets', 'app-123.js.map')));
});

test('missing configuration skips instead of failing local builds', async () => {
  const r = await uploadSourcemaps(build(), { project: 'web', silent: true, failOnError: false });
  assert.equal(r.skipped, true);
  await assert.rejects(uploadSourcemaps(build(), { project: 'web', silent: true, failOnError: true }), /VAKA_URL/);
});

test('vite plugin uses base and defines the release', async () => {
  const { srv, got, url } = await server();
  const dir = build();
  const p = vakaVite({ ...base, url });
  assert.deepEqual(p.config({}), { define: { __VAKA_RELEASE__: '"1.2.3"' } });
  p.configResolved({ root: dir, base: '/app/', command: 'build', build: { outDir: '.' } });
  await p.writeBundle.call({}, { dir });
  srv.close();
  assert.match(got[0].body, /\/app\/assets\/app-123\.js/);
});

test('webpack plugin uploads after emit with publicPath', async () => {
  const { srv, got, url } = await server();
  const dir = build();
  let hook;
  const defined = [];
  const compiler = {
    options: { mode: 'production', output: { publicPath: '/static/' } },
    outputPath: dir,
    webpack: { DefinePlugin: class { constructor(d) { defined.push(d); } apply() {} } },
    hooks: { afterEmit: { tapPromise: (_n, fn) => (hook = fn) } },
  };
  new VakaWebpackPlugin({ ...base, url }).apply(compiler);
  await hook({ outputOptions: { path: dir, publicPath: '/static/' } });
  srv.close();
  assert.deepEqual(defined, [{ __VAKA_RELEASE__: '"1.2.3"' }]);
  assert.match(got[0].body, /\/static\/assets\/app-123\.js/);
});

test('upload errors fail the build when failOnError is set', async () => {
  const { srv, url } = await server();
  const dir = build();
  fs.writeFileSync(path.join(dir, 'broken.js.map'), 'broken');
  await assert.rejects(uploadSourcemaps(dir, { ...base, url }), /1 source map upload\(s\) failed/);
  srv.close();
});
