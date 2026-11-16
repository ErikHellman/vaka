// A tiny stand-in for a Vaka server, for the e2e tests: `node tests/mock-vaka.mjs` listens on
// :18080 and answers the few endpoints the tests use. Token: vt_e2e.
import http from 'node:http';

const PORT = Number(process.env.MOCK_VAKA_PORT || 18080);

function series(params) {
  const since = Number(params.get('since')) || Date.now() - 3600_000;
  const until = Number(params.get('until')) || Date.now();
  const step = 300_000;
  const rows = [];
  for (let t = Math.floor(since / step) * step, i = 0; t <= until && rows.length < 2000; t += step, i++) {
    rows.push({
      bucket: new Date(t).toISOString().replace('.000Z', 'Z'),
      errors: (i % 5) + 1,
      users: 1,
      unhandled: 0,
      fatal: 0,
      issues: 1,
    });
  }
  return rows;
}

const routes = {
  '/api/v1/me': () => ({
    type: 'token',
    name: 'e2e',
    scope: 'read',
    server_version: '0.5.0',
    capabilities: ['errors_timeseries', 'stream'],
  }),
  '/api/v1/projects': () => ({
    projects: [
      { slug: 'web', name: 'Web shop' },
      { slug: 'app', name: 'Mobile app' },
    ],
  }),
  '/api/v1/projects/web': () => ({ slug: 'web', name: 'Web shop' }),
  '/api/v1/projects/web/releases': () => ({ releases: [{ version: '1.2.0', first_seen: new Date().toISOString() }] }),
  '/api/v1/projects/web/errors/timeseries': (p) => ({ interval: '5m', series: series(p) }),
  '/api/v1/projects/web/issues': () => ({
    issues: [
      {
        short_id: 'WEB-1',
        title: 'TypeError: x is undefined',
        level: 'error',
        status: 'open',
        events_in_window: 42,
        users_in_window: 7,
        url: 'http://localhost/ui',
      },
    ],
  }),
};

http
  .createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    res.setHeader('content-type', 'application/json');
    if (req.headers.authorization !== 'Bearer vt_e2e') {
      res.writeHead(401).end(JSON.stringify({ error: 'invalid or expired token' }));
      return;
    }
    const route = routes[url.pathname];
    if (!route) {
      res.writeHead(404).end(JSON.stringify({ error: 'not found' }));
      return;
    }
    res.end(JSON.stringify(route(url.searchParams)));
  })
  .listen(PORT, '0.0.0.0', () => console.log(`mock vaka on :${PORT}`));
