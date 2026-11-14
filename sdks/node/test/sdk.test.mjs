import test from 'node:test';
import assert from 'node:assert/strict';
import { fakeVaka, runFixture } from './helpers.mjs';

test('uncaught exception: reported as fatal with source context, then exit code 1', async () => {
  const v = await fakeVaka();
  const r = await runFixture('uncaught.mjs', v.endpoint);
  await v.close();
  assert.equal(r.code, 1, r.stderr);
  assert.match(r.stderr, /TypeError: kaboom/);
  const [e] = v.items().filter((i) => i.type === 'error');
  assert.ok(e, 'error sent');
  assert.equal(e.platform, 'node');
  assert.equal(e.fatal, true);
  assert.equal(e.handled, false);
  assert.equal(e.level, 'fatal');
  assert.equal(e.release, '1.2.3');
  assert.equal(e.environment, 'test');
  assert.equal(e.tags.mechanism, 'uncaughtException');
  assert.equal(e.exceptions[0].type, 'TypeError');
  assert.equal(e.exceptions[0].value, 'kaboom');
  const top = e.exceptions[0].stacktrace[0];
  assert.match(top.function, /explode$/);
  assert.equal(top.in_app, true);
  assert.match(top.context_line, /throw new TypeError\('kaboom'\)/);
  assert.ok(top.pre_context.length > 0);
  assert.ok(e.breadcrumbs.some((b) => b.message === 'about to fail'));
  assert.equal(e.contexts.runtime.name, 'node');
  const req = v.requests.find((q) => q.url === '/ingest/v1/batch');
  assert.equal(req.headers['x-vaka-key'], 'vk_test');
  assert.equal(req.body.sdk.name, 'vaka-node');
});

test('unhandled rejection: strict mode reports and exits like Node', async () => {
  const v = await fakeVaka();
  const r = await runFixture('rejection.mjs', v.endpoint);
  await v.close();
  assert.equal(r.code, 1);
  assert.doesNotMatch(r.stdout, /still alive/);
  const [e] = v.items();
  assert.equal(e.exceptions[0].value, 'nobody caught me');
  assert.equal(e.tags.mechanism, 'unhandledRejection');
  assert.equal(e.fatal, true);
});

test('unhandled rejection: --unhandled-rejections=warn keeps running', async () => {
  const v = await fakeVaka();
  const r = await runFixture('rejection.mjs', v.endpoint, { args: ['--unhandled-rejections=warn'] });
  await v.close();
  assert.equal(r.code, 0, r.stderr);
  assert.match(r.stdout, /still alive/);
  const [e] = v.items();
  assert.equal(e.fatal, false);
  assert.equal(e.level, 'error');
});

test('captureException, messages, events, flags and trace context', async () => {
  const v = await fakeVaka();
  const r = await runFixture('capture.cjs', v.endpoint);
  await v.close();
  assert.equal(r.code, 0, r.stderr);
  assert.match(r.stdout, /flag=one_page/);
  const items = v.items();
  const errors = items.filter((i) => i.type === 'error');
  const [e, msg, after] = errors;
  assert.equal(e.exceptions.length, 2, 'cause chain');
  assert.equal(e.exceptions[1].value, 'db timeout');
  assert.equal(e.tags.tenant, 'acme');
  assert.equal(e.tags.area, 'cart');
  assert.equal(e.tags.service, 'checkout');
  assert.equal(e.user_id, 'user-7');
  assert.equal(e.handled, true);
  assert.match(e.trace_id, /^[0-9a-f]{32}$/);
  assert.ok(e.breadcrumbs.some((b) => b.category === 'console' && b.message === 'disk almost full' && b.level === 'warning'));
  assert.equal(msg.message, 'hello');
  assert.equal(msg.level, 'warning');
  assert.deepEqual(after.flags, { new_checkout: 'one_page', beta: 'false' });
  const ev = items.find((i) => i.type === 'event');
  assert.equal(ev.name, 'order_paid');
  assert.deepEqual(ev.props, { amount: 49 });
  assert.equal(ev.user_id, 'user-7');
  const flagReqs = v.requests.filter((q) => q.url === '/ingest/v1/flags');
  assert.equal(flagReqs.length, 1, 'second evaluation is cached');
  assert.equal(flagReqs[0].body.distinct_id, 'user-7');
  // The span was exported over OTLP, marked so the server doesn't create a duplicate error.
  const otlp = v.requests.filter((q) => q.url === '/ingest/otlp/v1/traces');
  assert.ok(otlp.length >= 1, 'spans exported');
  assert.equal(otlp[0].headers['x-vaka-key'], 'vk_test');
  const raw = Buffer.concat(otlp.map((q) => q.body)).toString('latin1');
  assert.ok(raw.includes('vaka.error_captured'));
  assert.ok(raw.includes('feature_flag.new_checkout'));
  // The SDK's own requests are not breadcrumbs.
  assert.ok(!after.breadcrumbs.some((b) => b.category === 'http' && b.message.includes('/ingest/')));
});

test('express: per-request scope, 5xx captured, 4xx skipped, query stripped', async () => {
  const v = await fakeVaka();
  const r = await runFixture('express.cjs', v.endpoint);
  await v.close();
  assert.equal(r.code, 0, r.stderr);
  const errors = v.items().filter((i) => i.type === 'error');
  assert.equal(errors.length, 1, JSON.stringify(errors.map((e) => e.exceptions?.[0]?.value)));
  const [e] = errors;
  assert.equal(e.exceptions[0].value, 'user lookup failed');
  assert.equal(e.handled, false);
  assert.equal(e.user_id, 'bad');
  assert.equal(e.tags.transaction, 'GET /users/:id');
  assert.equal(e.contexts.request.method, 'GET');
  assert.match(e.url, /\/users\/bad$/);
  assert.equal(e.contexts.request.query_string, 'token=secret');
  const crumbs = e.breadcrumbs.map((b) => b.message);
  assert.ok(crumbs.includes('loading bad'));
  assert.ok(!crumbs.includes('loading good'), 'breadcrumbs isolated per request');
});

test('fastify plugin captures handler errors', async () => {
  const v = await fakeVaka();
  const r = await runFixture('fastify.mjs', v.endpoint);
  await v.close();
  assert.equal(r.code, 0, r.stderr);
  const errors = v.items().filter((i) => i.type === 'error');
  assert.equal(errors.length, 1);
  assert.equal(errors[0].exceptions[0].value, 'order broken');
  assert.equal(errors[0].tags.order, '9');
  assert.equal(errors[0].tags.transaction, 'GET /orders/:id');
  assert.equal(errors[0].tags.mechanism, 'fastify');
});

test('register preload: HTTP server spans with release and user go to OTLP', async () => {
  const v = await fakeVaka();
  const reg = new URL('../dist/register.mjs', import.meta.url).pathname;
  const r = await runFixture('otel-http.mjs', v.endpoint, { args: ['--import', reg], env: { OTEL_SERVICE_NAME: 'api' } });
  await v.close();
  assert.equal(r.code, 0, r.stderr);
  const raw = Buffer.concat(v.requests.filter((q) => q.url === '/ingest/otlp/v1/traces').map((q) => q.body)).toString('latin1');
  assert.ok(raw.includes('service.version') && raw.includes('1.2.3'), 'release on resource');
  assert.ok(raw.includes('deployment.environment.name'));
  assert.ok(raw.includes('enduser.id') && raw.includes('u-1'), 'user on server span');
  assert.ok(raw.includes('/hello'));
});

test('401 disables sending', async () => {
  const v = await fakeVaka({ status: 401 });
  const r = await runFixture('rejection.mjs', v.endpoint, { args: ['--unhandled-rejections=none'] });
  await v.close();
  assert.equal(r.code, 0);
  assert.equal(v.requests.filter((q) => q.url === '/ingest/v1/batch').length, 1);
});

test('captureFeedback: JSON and multipart, scope data, links, errors', async () => {
  const v = await fakeVaka();
  const r = await runFixture('feedback.mjs', v.endpoint);
  await v.close();
  assert.equal(r.code, 0, r.stderr);
  assert.match(r.stdout, /result=\{"id":"fb-1","shortId":"API-F1"\}/);
  assert.match(r.stdout, /empty=FeedbackError/);
  assert.match(r.stdout, /many=at most 3 attachments/);
  const fb = v.requests.filter((q) => q.url === '/ingest/v1/feedback');
  assert.equal(fb.length, 2);
  const [json, multi] = fb;
  assert.equal(json.headers['x-vaka-key'], 'vk_test');
  assert.equal(json.headers['x-forwarded-for'], '203.0.113.9');
  const b = json.body;
  assert.equal(b.message, 'The export button does nothing');
  assert.equal(b.email, 'ann@example.com');
  assert.equal(b.issue, 'API-12');
  assert.equal(b.session_id, 's-1');
  assert.equal(b.user_id, 'user-7');
  assert.equal(b.source, 'api');
  assert.equal(b.platform, 'node');
  assert.equal(b.release, '1.2.3');
  assert.deepEqual(b.tags, { tenant: 'acme', plan: 'pro' });
  const err = v.items().find((i) => i.type === 'error');
  assert.equal(b.event_id, err.event_id);
  assert.match(multi.headers['content-type'], /^multipart\/form-data/);
  const raw = multi.body.toString('latin1');
  assert.match(raw, /name="feedback"\r\n\r\n\{[^\r]*"message":"with a screenshot"/);
  assert.match(raw, /filename="shot.png"\r\nContent-Type: image\/png/);
});

test('withMonitor and checkIn: run lifecycle, error capture, upsert config', async () => {
  const v = await fakeVaka();
  const r = await runFixture('monitor.mjs', v.endpoint);
  await v.close();
  assert.equal(r.code, 0, r.stderr);
  assert.match(r.stdout, /result=42/);
  assert.match(r.stdout, /rethrown=sync broke/);
  assert.match(r.stdout, /plain=true/);
  assert.match(r.stdout, /refused=false/);
  const cis = v.requests.filter((q) => q.url.startsWith('/ingest/v1/monitors/'));
  const backup = cis.filter((q) => q.url === '/ingest/v1/monitors/nightly-backup/check-ins');
  assert.deepEqual(backup.map((q) => q.body.status), ['in_progress', 'ok']);
  assert.equal(backup[0].headers['x-vaka-key'], 'vk_test');
  assert.deepEqual(backup[0].body.monitor, { schedule: { cron: '0 3 * * *', timezone: 'Europe/Stockholm' }, grace_s: 600, max_runtime_s: 3600 });
  assert.equal(backup[0].body.check_in_id, backup[1].body.check_in_id);
  assert.equal(backup[1].body.release, '1.2.3');
  assert.ok(backup[1].body.duration_ms >= 0);
  const sync = cis.filter((q) => q.url === '/ingest/v1/monitors/sync/check-ins');
  assert.deepEqual(sync.map((q) => q.body.status), ['in_progress', 'error']);
  assert.equal(sync[1].body.message, 'sync broke');
  const err = v.items().find((i) => i.type === 'error');
  assert.equal(err.tags['monitor.check_in_id'], sync[0].body.check_in_id);
  assert.equal(err.tags['monitor.slug'], 'sync');
  assert.match(r.stderr, /check-in for missing failed: HTTP 404/);
});

test('captureFeedback: rejected feedback throws with the status', async () => {
  const v = await fakeVaka({ feedbackStatus: 429 });
  const r = await runFixture('feedback.mjs', v.endpoint, { env: { EXPECT_429: '1' } });
  await v.close();
  assert.match(r.stdout, /status=429 vaka feedback: HTTP 429: too much feedback/);
});
