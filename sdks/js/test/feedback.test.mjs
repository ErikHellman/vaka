import { test } from 'node:test';
import assert from 'node:assert/strict';
import { init, identify, captureError, captureFeedback, lastEventId, onUnhandledError, _reset } from '../dist/vaka.mjs';
import { installFeedback, showFeedback, captureScreenshot } from '../dist/feedback.mjs';

function mockFetch(calls, status = 202) {
  globalThis.fetch = async (url, req) => {
    calls.push({ url, req });
    if (url.endsWith('/ingest/v1/feedback')) return { status, json: async () => ({ id: 'f1', short_id: 'WEB-F1' }) };
    return { status: 202 };
  };
}

test('captureFeedback sends JSON with context and returns the ids', async () => {
  const calls = [];
  mockFetch(calls);
  _reset();
  init({ key: 'vk_test', endpoint: 'https://vaka.example.com/', release: '1.2.3' });
  identify('u42');
  const res = await captureFeedback({ message: 'The export button does nothing', email: ' a@b.se ', tags: { plan: 'pro' } });
  assert.deepEqual(res, { id: 'f1', short_id: 'WEB-F1' });
  const c = calls.find((x) => x.url === 'https://vaka.example.com/ingest/v1/feedback');
  assert.equal(c.req.headers['X-Vaka-Key'], 'vk_test');
  const body = JSON.parse(c.req.body);
  assert.equal(body.message, 'The export button does nothing');
  assert.equal(body.email, 'a@b.se');
  assert.equal(body.release, '1.2.3');
  assert.equal(body.user_id, 'u42');
  assert.equal(body.source, 'api');
  assert.deepEqual(body.tags, { plan: 'pro' });
  assert.match(body.id, /^[0-9a-f-]{36}$/);
});

test('captureFeedback uploads attachments as multipart and ignores empty messages', async () => {
  const calls = [];
  mockFetch(calls);
  _reset();
  init({ key: 'vk_test', endpoint: 'https://vaka.example.com' });
  assert.equal(await captureFeedback({ message: '   ' }), undefined);
  const png = new Blob([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], { type: 'image/png' });
  await captureFeedback({ message: 'see screenshot', source: 'widget', attachments: [png] });
  const c = calls.find((x) => x.url.endsWith('/ingest/v1/feedback'));
  assert.ok(c.req.body instanceof FormData);
  assert.equal(JSON.parse(c.req.body.get('feedback')).source, 'widget');
  assert.equal(c.req.body.get('screenshot').name, 'screenshot-1.png');
  assert.equal(c.req.headers['Content-Type'], undefined, 'browser sets the multipart boundary');
});

test('captureFeedback resolves undefined when rejected or before init', async () => {
  const calls = [];
  mockFetch(calls, 429);
  _reset();
  assert.equal(await captureFeedback({ message: 'hi' }), undefined);
  init({ key: 'vk_test', endpoint: 'https://vaka.example.com' });
  assert.equal(await captureFeedback({ message: 'hi' }), undefined);
});

test('lastEventId and unhandled-error listeners link crash feedback', () => {
  mockFetch([]);
  _reset();
  init({ key: 'vk_test', endpoint: 'https://vaka.example.com' });
  const seen = [];
  const off = onUnhandledError((id) => seen.push(id));
  const handled = captureError(new Error('caught'));
  assert.equal(lastEventId(), handled);
  assert.deepEqual(seen, []);
  const crash = captureError(new Error('boom'), { handled: false });
  assert.deepEqual(seen, [crash]);
  off();
  captureError(new Error('again'), { handled: false });
  assert.equal(seen.length, 1);
});

test('widget functions are safe without a DOM', async () => {
  installFeedback({ crashPrompt: true });
  await showFeedback();
  assert.equal(await captureScreenshot(), undefined);
});
