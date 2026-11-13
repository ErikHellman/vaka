import * as Vaka from '../../dist/index.mjs';
Vaka.init({ otel: false });
Promise.reject(new Error('nobody caught me'));
setTimeout(() => console.log('still alive'), 500);
