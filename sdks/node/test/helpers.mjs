import http from 'node:http';
import { gunzipSync } from 'node:zlib';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

/** A fake Vaka server that records every request. */
export async function fakeVaka({ status = 202, flags = { new_checkout: 'one_page', beta: false } } = {}) {
  const requests = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      let buf = Buffer.concat(chunks);
      if (req.headers['content-encoding'] === 'gzip') buf = gunzipSync(buf);
      const isJson = !req.url.startsWith('/ingest/otlp');
      requests.push({ method: req.method, url: req.url, headers: req.headers, body: isJson && buf.length ? JSON.parse(buf.toString()) : buf });
      if (req.url === '/ingest/v1/flags') {
        res.writeHead(200, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ flags }));
      }
      res.writeHead(req.url.startsWith('/ingest/otlp') ? 200 : status, { 'content-type': 'application/json' });
      res.end('{}');
    });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const endpoint = `http://127.0.0.1:${server.address().port}`;
  return {
    endpoint,
    requests,
    items: () => requests.filter((r) => r.url === '/ingest/v1/batch').flatMap((r) => r.body.items),
    close: () => new Promise((r) => server.close(r)),
  };
}

/** Run a fixture script in a fresh Node process with Vaka env pointing at `endpoint`. */
export function runFixture(name, endpoint, { env = {}, args = [] } = {}) {
  const file = fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url));
  return new Promise((resolve) => {
    const p = spawn(process.execPath, [...args, file], {
      env: { ...process.env, VAKA_KEY: 'vk_test', VAKA_ENDPOINT: endpoint, VAKA_RELEASE: '1.2.3', NODE_ENV: 'test', ...env },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    p.stdout.on('data', (d) => (stdout += d));
    p.stderr.on('data', (d) => (stderr += d));
    p.on('exit', (code) => resolve({ code, stdout, stderr }));
  });
}
