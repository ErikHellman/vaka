// Preloaded with --import ../../dist/register.mjs
import http from 'node:http';
import * as Vaka from '../../dist/index.mjs';
const server = http.createServer((req, res) => {
  Vaka.setUser('u-1');
  res.end('hi');
});
server.listen(0, async () => {
  await fetch(`http://127.0.0.1:${server.address().port}/hello`);
  server.close();
  await Vaka.close();
});
