import * as Vaka from '../../dist/index.mjs';
Vaka.init();
Vaka.addBreadcrumb({ category: 'app', message: 'about to fail' });
function explode() {
  throw new TypeError('kaboom');
}
setTimeout(explode, 10);
