import * as Vaka from '../../dist/index.mjs';
Vaka.init({ otel: false });
const { default: Fastify } = await import('fastify');
const app = Fastify();
await app.register(Vaka.fastifyPlugin);
app.get('/orders/:id', async (req) => {
  Vaka.setTag('order', req.params.id);
  throw new Error('order broken');
});
app.get('/ok', async () => ({ ok: true }));
await app.listen({ port: 0, host: '127.0.0.1' });
const base = `http://127.0.0.1:${app.server.address().port}`;
await fetch(`${base}/ok`);
await fetch(`${base}/orders/9`);
await Vaka.close();
await app.close();
