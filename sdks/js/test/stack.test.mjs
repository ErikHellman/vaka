import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseStack } from '../dist/vaka.mjs';

test('parses chrome stacks', () => {
  const s = `TypeError: x is undefined
    at render (https://shop.example.com/assets/app-1a2b.js:1:2345)
    at https://shop.example.com/assets/app-1a2b.js:1:99
    at async load (https://cdn.example.com/node_modules/lib.js:3:4)`;
  const f = parseStack(s, 'https://shop.example.com');
  assert.equal(f.length, 3);
  assert.deepEqual(f[0], { function: 'render', abs_path: 'https://shop.example.com/assets/app-1a2b.js', filename: 'app-1a2b.js', lineno: 1, colno: 2345, in_app: true });
  assert.equal(f[1].function, '?');
  assert.equal(f[2].function, 'load');
  assert.equal(f[2].in_app, false);
});

test('parses firefox and safari stacks', () => {
  const s = `render@https://shop.example.com/assets/app.js:10:5
@https://shop.example.com/assets/app.js:2:1
[native code]`;
  const f = parseStack(s, 'https://shop.example.com');
  assert.equal(f.length, 2);
  assert.equal(f[0].function, 'render');
  assert.equal(f[0].lineno, 10);
  assert.equal(f[0].colno, 5);
  assert.equal(f[1].function, '?');
});

test('ignores garbage', () => {
  assert.deepEqual(parseStack('not a stack\nreally'), []);
  assert.deepEqual(parseStack(undefined), []);
});
