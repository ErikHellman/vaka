const Vaka = require('../../dist/index.cjs');
const { trace } = require('@opentelemetry/api');
Vaka.init({ serviceName: 'checkout', debug: false });
Vaka.setTag('tenant', 'acme');
Vaka.setUser('user-7');
console.warn('disk almost full');
const tracer = trace.getTracer('test');
tracer.startActiveSpan('work', async (span) => {
  const cause = new Error('db timeout');
  const err = new Error('checkout failed', { cause });
  Vaka.captureException(err, { tags: { area: 'cart' } });
  Vaka.captureMessage('hello', 'warning');
  Vaka.track('order_paid', { amount: 49, coupon: null });
  const v = await Vaka.getFlag('new_checkout');
  await Vaka.getFlag('new_checkout'); // cached
  console.log('flag=' + v);
  Vaka.captureException(new Error('after flags'));
  span.end();
  await Vaka.close();
});
