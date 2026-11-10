import { build } from 'esbuild';

const common = { bundle: true, target: 'es2018', legalComments: 'none' };
await build({ ...common, entryPoints: ['src/index.ts'], format: 'esm', outfile: 'dist/vaka.mjs' });
await build({ ...common, entryPoints: ['src/index.ts'], format: 'cjs', outfile: 'dist/vaka.cjs' });
// Script-tag build: exposes window.Vaka and auto-initializes from data attributes.
await build({ ...common, entryPoints: ['src/script.ts'], format: 'iife', minify: true, outfile: 'dist/vaka.min.js' });
