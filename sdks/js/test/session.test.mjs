import { test } from 'node:test';
import assert from 'node:assert/strict';
import { init, track, flush, sessionId, _reset } from '../dist/vaka.mjs';

test('events carry a stable session id', async () => {
  const bodies = [];
  globalThis.fetch = async (_url, req) => {
    bodies.push(JSON.parse(req.body));
    return { status: 202 };
  };
  _reset();
  init({ key: 'vk_test', endpoint: 'https://vaka.example.com' });
  const sid = sessionId();
  assert.match(sid, /^[0-9a-f-]{36}$/);
  track('signup', { plan: 'pro' });
  track('upgrade');
  await flush();
  const items = bodies.flatMap((b) => b.items);
  assert.equal(items.length, 2);
  assert.deepEqual(items.map((i) => i.session_id), [sid, sid]);
});

test('sessions can be turned off', async () => {
  _reset();
  init({ key: 'vk_test', endpoint: 'https://vaka.example.com', sessions: false });
  assert.equal(sessionId(), undefined);
});
