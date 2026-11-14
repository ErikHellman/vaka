import * as Vaka from '../../dist/index.mjs';
Vaka.init({ otel: false });
const v = await Vaka.withMonitor('nightly-backup', async () => 42, { schedule: { cron: '0 3 * * *', timezone: 'Europe/Stockholm' }, graceS: 600, maxRuntimeS: 3600 });
console.log('result=' + v);
try {
  await Vaka.withMonitor('sync', async () => {
    throw new Error('sync broke');
  });
} catch (e) {
  console.log('rethrown=' + e.message);
}
console.log('plain=' + (await Vaka.checkIn('heartbeat', 'ok')));
console.log('refused=' + (await Vaka.checkIn('missing', 'ok')));
await Vaka.close();
