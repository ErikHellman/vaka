import { test } from 'node:test';
import assert from 'node:assert/strict';
import { init, track, flush, loadFlags, getFlag, isEnabled, onFlags, _reset } from '../dist/vaka.mjs';

test('flags are loaded, read and attached to items', async () => {
  const calls = [];
  globalThis.fetch = async (url, req) => {
    calls.push({ url, body: JSON.parse(req.body) });
    if (url.endsWith('/ingest/v1/flags')) return { status: 200, json: async () => ({ flags: { new_checkout: true, pricing: 'b', off_flag: false } }) };
    return { status: 202 };
  };
  _reset();
  init({ key: 'vk_test', endpoint: 'https://vaka.example.com', distinctId: 'device-1' });
  assert.equal(getFlag('pricing', 'a'), 'a', 'fallback before load');
  let seen;
  onFlags((f) => (seen = f));
  const flags = await loadFlags({ plan: 'pro' });
  assert.equal(flags.pricing, 'b');
  assert.deepEqual(seen, flags);
  assert.equal(calls[0].body.distinct_id, 'device-1');
  assert.deepEqual(calls[0].body.properties, { plan: 'pro' });
  assert.equal(isEnabled('new_checkout'), true);
  assert.equal(isEnabled('off_flag'), false);
  assert.equal(isEnabled('missing'), false);
  track('checkout');
  await flush();
  const item = calls.flatMap((c) => c.body.items || [])[0];
  assert.deepEqual(item.flags, { new_checkout: 'true', pricing: 'b', off_flag: 'false' });
});
