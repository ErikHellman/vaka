# User feedback

End users can tell you what went wrong or what they want, in their own words. Feedback is
stored next to your crashes, linked to the crash it is about, tagged with sentiment and topics,
and readable by agents through the [REST API](api.md#user-feedback) and the
[MCP server](mcp.md).

- **Widget**: a floating "Feedback" button for web apps (Android and iOS widgets are coming),
  with an optional screenshot and email field.
- **Crash prompt**: after an uncaught error, ask "what were you doing?". The answer is linked to
  that crash's issue.
- **API**: `captureFeedback()` in the SDKs, or `POST /ingest/v1/feedback` from anywhere.
- **Sentiment and topics**: when an LLM is configured (`VAKA_LLM_*`, see
  [self-hosting](self-hosting.md#configuration)), each item gets a sentiment, 1 to 3 topics and a
  one-line summary. Without an LLM, feedback works the same, untagged.
- **Triage**: status `new`, `triaged`, `resolved` or `ignored`, an assignee, and a link to an
  issue. A `new_feedback` [alert trigger](alerts.md) and a feedback block in digests.

## Web widget

With the script tag, load the widget after `vaka.js`:

```html
<script defer src="{{ENDPOINT}}/sdk/vaka.js" data-key="vk_..."></script>
<script defer src="{{ENDPOINT}}/sdk/vaka-feedback.js" data-crash-prompt="true"></script>
```

Attributes: `data-button="false"` (no floating button; open it yourself),
`data-position="bottom-left"`, `data-crash-prompt="true"`, `data-screenshot="false"`,
`data-ask-email="false"`, `data-accent="#16a34a"`, `data-auto="false"` (don't install; call
`window.VakaFeedback.installFeedback(...)` yourself).

With npm:

```ts
import { init } from '@hellsoft/vaka-browser';
import { installFeedback, showFeedback } from '@hellsoft/vaka-browser/feedback';

init({ key: 'vk_...', endpoint: '{{ENDPOINT}}' });
installFeedback({ crashPrompt: true, email: currentUser?.email });

// Your own "Report a problem" link:
document.querySelector('#report')!.addEventListener('click', () => showFeedback());
```

See the [JavaScript SDK](sdk-javascript.md#user-feedback) for every option. The widget lives in
a shadow root, so page styles don't leak in or out.

### Screenshots

The widget renders the visible part of the page to a JPEG in the browser (no extension or
permission prompt). The user sees it and can untick it before sending. Password fields and
anything marked `data-vaka-mask` are blacked out; canvases, videos and iframes appear as empty
boxes. Turn screenshots off with `screenshot: false`.

## Sending feedback from code

```ts
import { captureFeedback, lastEventId } from '@hellsoft/vaka-browser';

const r = await captureFeedback({ message: 'The export button does nothing', email, eventId: lastEventId() });
// r = { id, short_id: 'WEB-F12' }
```

### From a backend

The server SDKs have no widget. They forward feedback your users wrote in your own forms
(support, contact, in-app chat), right away, with the release, environment and the request
scope's user, tags and flags:

| SDK | Call |
| --- | --- |
| [Node.js](sdk-node.md#user-feedback) | `await Vaka.captureFeedback({ message, email, issue, ip })` |
| [Python](sdk-python.md#user-feedback) | `vaka.capture_feedback(message, email=..., issue=..., ip=...)` |
| [Go](sdk-go.md#user-feedback) | `vaka.CaptureFeedback(ctx, vaka.Feedback{Message: ..., IP: ...})` |
| [Rust](sdk-rust.md#user-feedback) | `vaka_sdk::capture_feedback(vaka_sdk::Feedback { .. })` |
| [JVM](sdk-jvm.md#user-feedback) | `Vaka.captureFeedback(Feedback.of(message).email(...).ip(...))` |
| [.NET](sdk-dotnet.md#user-feedback) | `await VakaSdk.CaptureFeedbackAsync(new VakaFeedback(message) { ... })` |

- Link feedback to a crash with `eventId` (the id the capture call returned) or to an issue
  with `issue` (`API-12`).
- Pass the end user's IP address. The SDK sends it as `X-Forwarded-For`, so the per-address
  rate limit applies to each user instead of to your server. Vaka only believes that header from
  [trusted proxies](self-hosting.md#configuration): backends on a private network (the same
  Docker network, VPC or cluster) are trusted by default; add a backend that calls Vaka from a
  public address to `VAKA_TRUSTED_PROXIES`. Otherwise all feedback from that server shares one
  limit (raise `VAKA_FEEDBACK_PER_IP_HOUR` if needed). Browsers can't use the header to dodge
  the limit.
- Up to 3 images go as a multipart upload.
- The call returns `{ id, short_id }`, and fails with the HTTP status when the server refuses
  the feedback (`429` for a rate limit), so your form can tell the user.

Anything else can send it over the [ingest protocol](ingest.md#feedback-item).

## Limits

- Message: required, at most 5000 characters.
- Up to 3 images (PNG, JPEG or WebP), 5 MB and 4096 px per side each. EXIF data is stripped
  from JPEGs.
- Rate limits per project: 10 per hour per IP address and 5 per hour per session
  (`VAKA_FEEDBACK_PER_IP_HOUR`, `VAKA_FEEDBACK_PER_SESSION_HOUR`). Extra sends get `429`.
- Retries with the same client `id` are stored once.

## Reading feedback

| Where | How |
| --- | --- |
| MCP | `list_feedback`, `get_feedback` (with screenshots as images), `feedback_summary`, `update_feedback` |
| REST | `GET /api/v1/projects/{p}/feedback`, `GET /api/v1/projects/{p}/feedback/summary`, `GET` / `PATCH /api/v1/feedback/{id}` |
| Admin UI | `/ui/projects/{slug}/feedback`: list with filters, detail page with screenshots and status |

Feedback also shows up where it is useful: `get_issue` and `get_fix_context` include the
feedback linked to an issue, session and user timelines include it in order, `compare_releases`
compares feedback volume and sentiment, and `flag_health` splits it by variant.

`feedback_summary` groups by `sentiment`, `topic`, `release`, `flag`, `day`, `status`,
`platform` or `source`, with counts, the sentiment split, the negative rate and a few quotes per
group. Grouping feedback into themes beyond topics is left to the agent reading it.

Example prompts:

- "What are users complaining about since the 2.4 release?"
- "Summarize negative feedback about checkout this week and link each item to an issue."
- "Does the new_checkout variant get worse feedback than control?"

## Privacy

- The message goes through the same scrubber as crash reports, except that email addresses are
  kept (the user typed them on purpose). Card numbers and secrets are filtered.
- The contact email is optional and only visible to write tokens and admins. It is never sent in
  alerts, digests or to the LLM.
- Only the message, platform and URL path go to the LLM, never the email, user id or screenshots.
- Vaka never emails end users.
- Feedback follows the project's retention period, and `delete_user_data` deletes it, matching
  the user id or the contact email. See [privacy](privacy.md#user-feedback).
