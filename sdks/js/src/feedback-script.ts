// Script-tag entry point for the feedback widget; load it after vaka.js:
// <script defer src="https://vaka.example.com/sdk/vaka-feedback.js" data-crash-prompt="true"></script>
// Attributes: data-button="false", data-position="bottom-left", data-crash-prompt="true",
// data-screenshot="false", data-ask-email="false", data-accent="#16a34a".
import { installFeedback, showFeedback, uninstallFeedback, captureScreenshot } from './feedback';

declare global {
  interface Window {
    VakaFeedback: { installFeedback: typeof installFeedback; showFeedback: typeof showFeedback; uninstallFeedback: typeof uninstallFeedback; captureScreenshot: typeof captureScreenshot };
  }
}

window.VakaFeedback = { installFeedback, showFeedback, uninstallFeedback, captureScreenshot };
const script = document.currentScript as HTMLScriptElement | null;
if (script && script.dataset.auto !== 'false') {
  const d = script.dataset;
  installFeedback({
    button: d.button !== 'false',
    position: d.position === 'bottom-left' ? 'bottom-left' : 'bottom-right',
    crashPrompt: d.crashPrompt === 'true',
    screenshot: d.screenshot !== 'false',
    askEmail: d.askEmail !== 'false',
    accent: d.accent,
  });
}
