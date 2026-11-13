import { build } from 'esbuild';
import { readFileSync } from 'node:fs';

const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url)));
const external = Object.keys(pkg.dependencies || {});
const common = { logOverride: { 'empty-import-meta': 'silent' }, bundle: true, platform: 'node', target: 'node22', legalComments: 'none', external };
// register imports the built index so a process has one SDK instance per module format.
const ownIndex = (ext) => ({
  name: 'own-index',
  setup(b) {
    b.onResolve({ filter: /^\.\/index$/ }, () => ({ path: `./index.${ext}`, external: true }));
  },
});
for (const [format, ext] of [['esm', 'mjs'], ['cjs', 'cjs']]) {
  await build({ ...common, entryPoints: ['src/index.ts'], format, outfile: `dist/index.${ext}` });
  await build({ ...common, entryPoints: ['src/register.ts'], format, outfile: `dist/register.${ext}`, plugins: [ownIndex(ext)] });
}
