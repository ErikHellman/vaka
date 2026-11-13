import { test } from 'node:test';
import assert from 'node:assert/strict';
import { init, flush, startSpan, captureError, traceId, _reset } from '../dist/vaka.mjs';

test('startSpan sends a span item with ids and duration', async () => {
  const bodies = [];
  globalThis.fetch = async (_url, req) => {
    bodies.push(JSON.parse(req.body));
    return { status: 202 };
  };
  _reset();
  init({ key: 'vk_test', endpoint: 'https://vaka.example.com', release: '1.0.0' });
  assert.equal(traceId(), undefined);
  const span = startSpan({ name: 'render cart', op: 'ui.render', attributes: { items: 3 } });
  assert.match(span.traceId, /^[0-9a-f]{32}$/);
  assert.match(span.spanId, /^[0-9a-f]{16}$/);
  span.setMeasurement('rows', 3);
  span.end();
  span.end(); // second end is ignored
  captureError(new Error('boom'));
  await flush();
  const items = bodies.flatMap((b) => b.items);
  const spans = items.filter((i) => i.type === 'span');
  assert.equal(spans.length, 1);
  const s = spans[0];
  assert.equal(s.name, 'render cart');
  assert.equal(s.op, 'ui.render');
  assert.equal(s.status, 'ok');
  assert.equal(s.release, '1.0.0');
  assert.equal(s.parent_span_id, undefined);
  assert.ok(s.timestamp >= s.start_timestamp);
  assert.deepEqual(s.attributes, { items: 3 });
  assert.deepEqual(s.measurements, { rows: 3 });
  const err = items.find((i) => i.type === 'error');
  assert.equal(err.trace_id, undefined, 'untraced page: errors carry no trace id');
});
