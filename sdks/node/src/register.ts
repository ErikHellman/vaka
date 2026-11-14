/**
 * Preload entry: initializes Vaka from environment variables before the app loads, so
 * OpenTelemetry can instrument `http`, Express and friends.
 *
 *   VAKA_KEY=vk_... VAKA_ENDPOINT=https://vaka.example.com node --import @hellsoft/vaka-node/register app.js
 *   (CommonJS: node --require @hellsoft/vaka-node/register app.js)
 */
import * as nodeModule from 'node:module';
import { pathToFileURL } from 'node:url';
import { init } from './index';

// ES modules are only patched through a loader hook; CommonJS needs none.
const register = (nodeModule as { register?: (specifier: string, parent: string) => void }).register;
if (register && process.env.VAKA_ESM_HOOK !== '0') {
  try {
    const parent = typeof __filename !== 'undefined' ? pathToFileURL(__filename).href : import.meta.url;
    register('@opentelemetry/instrumentation/hook.mjs', parent);
  } catch {
    /* older Node: CommonJS instrumentation still works */
  }
}

init({
  tracesSampleRate: process.env.VAKA_TRACES_SAMPLE_RATE ? Number(process.env.VAKA_TRACES_SAMPLE_RATE) : undefined,
  debug: process.env.VAKA_DEBUG === '1' || process.env.VAKA_DEBUG === 'true',
});
