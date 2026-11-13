import test from 'node:test';
import assert from 'node:assert/strict';
import { parseStack } from '../dist/index.mjs';

test('parses V8 frames, marks vendor and internal frames', () => {
  const stack = [
    'TypeError: x is not a function',
    '    at Cart.checkout (/srv/app/src/cart.js:42:13)',
    '    at async handler (file:///srv/app/src/routes/orders.mjs:7:5)',
    '    at Layer.handle [as handle_request] (/srv/app/node_modules/express/lib/router/layer.js:95:5)',
    '    at /srv/app/node_modules/@scope/pkg/index.js:1:2',
    '    at process.processTicksAndRejections (node:internal/process/task_queues:95:5)',
    '    at new Promise (<anonymous>)',
  ].join('\n');
  const f = parseStack(stack, '/srv/app');
  assert.equal(f.length, 6);
  assert.deepEqual(f[0], { function: 'Cart.checkout', module: 'src/cart', abs_path: '/srv/app/src/cart.js', filename: 'src/cart.js', lineno: 42, colno: 13, in_app: true });
  assert.equal(f[1].abs_path, '/srv/app/src/routes/orders.mjs');
  assert.equal(f[1].function, 'handler');
  assert.equal(f[2].function, 'Layer.handle');
  assert.equal(f[2].module, 'express:lib/router/layer');
  assert.equal(f[2].in_app, false);
  assert.equal(f[3].module, '@scope/pkg:index');
  assert.equal(f[4].in_app, false);
  assert.equal(f[4].module, 'internal/process/task_queues');
  assert.equal(f[5].function, 'Promise');
  assert.equal(f[5].in_app, false);
});

test('parses Windows paths', () => {
  const f = parseStack('Error: x\n    at run (C:\\app\\src\\main.js:3:9)', 'C:\\app');
  assert.equal(f[0].filename, 'src/main.js');
  assert.equal(f[0].in_app, true);
});
