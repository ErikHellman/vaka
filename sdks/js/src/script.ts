// Script-tag entry point:
// <script defer src="https://vaka.example.com/sdk/vaka.js" data-key="vk_..." data-release="1.2.3"></script>
// Optional: data-traces-sample-rate="0.1" for page load, fetch and Web Vitals spans.
import * as Vaka from './index';

declare global {
  interface Window {
    Vaka: typeof Vaka;
  }
}

window.Vaka = Vaka;
const script = document.currentScript as HTMLScriptElement | null;
if (script && script.dataset.key) {
  const d = script.dataset;
  Vaka.init({
    key: d.key as string,
    endpoint: d.endpoint || new URL(script.src).origin,
    release: d.release,
    environment: d.environment,
    autoPageviews: d.pageviews !== 'false',
    hashRouting: d.hash === 'true',
    captureErrors: d.errors !== 'false',
    ignoreLocalhost: d.ignoreLocalhost === 'true',
    tracesSampleRate: d.tracesSampleRate ? Number(d.tracesSampleRate) : undefined,
    debug: d.debug === 'true',
  });
}
