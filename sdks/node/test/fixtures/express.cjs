const Vaka = require('../../dist/index.cjs');
Vaka.init({ otel: false });
const express = require('express');
const app = express();
app.use(Vaka.requestHandler());
app.get('/users/:id', (req, res, next) => {
  Vaka.setUser(req.params.id);
  Vaka.addBreadcrumb({ category: 'app', message: 'loading ' + req.params.id });
  if (req.params.id === 'bad') return next(new Error('user lookup failed'));
  res.send('ok');
});
app.get('/missing', (req, res, next) => {
  const e = new Error('not found');
  e.status = 404;
  next(e);
});
app.use(Vaka.errorHandler());
app.use((err, req, res, next) => res.status(err.status || 500).send('error'));
const server = app.listen(0, async () => {
  const base = `http://127.0.0.1:${server.address().port}`;
  await fetch(`${base}/users/good`);
  await fetch(`${base}/missing`);
  await fetch(`${base}/users/bad?token=secret`);
  await Vaka.close();
  server.close();
});
