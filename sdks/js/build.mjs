import { build } from 'esbuild';

const common = { bundle: true, target: 'es2018', legalComments: 'none' };
await build({ ...common, entryPoints: ['src/index.ts'], format: 'esm', outfile: 'dist/vaka.mjs' });
await build({ ...common, entryPoints: ['src/index.ts'], format: 'cjs', outfile: 'dist/vaka.cjs' });
// Script-tag build: exposes window.Vaka and auto-initializes from data attributes.
await build({ ...common, entryPoints: ['src/script.ts'], format: 'iife', minify: true, outfile: 'dist/vaka.min.js' });

// The feedback widget is a separate entry so the core bundle stays small. It must share the core's
// state, so its import of './index' resolves to the published package (ESM/CJS) or to window.Vaka
// (script tag) instead of a second bundled copy.
const core = (target) => ({
  name: 'vaka-core',
  setup(b) {
    b.onResolve({ filter: /^\.\/index$/ }, () => (target === 'global' ? { path: 'vaka-core', namespace: 'vaka-global' } : { path: target, external: true }));
    b.onLoad({ filter: /.*/, namespace: 'vaka-global' }, () => ({ contents: 'module.exports = window.Vaka', loader: 'js' }));
  },
});
await build({ ...common, entryPoints: ['src/feedback.ts'], format: 'esm', outfile: 'dist/feedback.mjs', plugins: [core('./vaka.mjs')] });
await build({ ...common, entryPoints: ['src/feedback.ts'], format: 'cjs', outfile: 'dist/feedback.cjs', plugins: [core('./vaka.cjs')] });
await build({ ...common, entryPoints: ['src/feedback-script.ts'], format: 'iife', minify: true, outfile: 'dist/vaka-feedback.min.js', plugins: [core('global')] });
