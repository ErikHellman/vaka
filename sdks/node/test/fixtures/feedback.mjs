import * as Vaka from '../../dist/index.mjs';
Vaka.init({ otel: false });
Vaka.setTag('tenant', 'acme');
Vaka.setUser('user-7');
if (process.env.EXPECT_429) {
  try {
    await Vaka.captureFeedback({ message: 'again' });
  } catch (e) {
    console.log('status=' + e.status + ' ' + e.message);
  }
  process.exit(0);
}
const eventId = Vaka.captureException(new Error('export failed'));
const r = await Vaka.captureFeedback({
  message: '  The export button does nothing  ',
  email: 'ann@example.com',
  eventId,
  issue: 'API-12',
  sessionId: 's-1',
  ip: '203.0.113.9',
  tags: { plan: 'pro' },
});
console.log('result=' + JSON.stringify(r));
const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
await Vaka.captureFeedback({ message: 'with a screenshot', attachments: [{ filename: 'shot.png', data: png }] });
try {
  await Vaka.captureFeedback({ message: '   ' });
} catch (e) {
  console.log('empty=' + e.name);
}
try {
  await Vaka.captureFeedback({ message: 'x', attachments: [1, 2, 3, 4].map(() => ({ filename: 'a.png', data: png })) });
} catch (e) {
  console.log('many=' + e.message);
}
await Vaka.close();
