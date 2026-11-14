# @hellsoft/vaka-node

Node.js SDK for [Vaka](https://github.com/ErikHellman/vaka): crash and error reporting,
OpenTelemetry traces and logs, breadcrumbs, release tagging and feature flags.

```sh
npm install @hellsoft/vaka-node
VAKA_KEY=vk_... VAKA_ENDPOINT=https://vaka.example.com VAKA_RELEASE=1.0.0 \
  node --import @hellsoft/vaka-node/register server.js
```

```ts
import * as Vaka from '@hellsoft/vaka-node';

app.use(Vaka.requestHandler());   // Express: per-request scope
// routes
app.use(Vaka.errorHandler());     // report 5xx errors

Vaka.setUser(req.user.id);
if (await Vaka.isEnabled('new_checkout')) { /* ... */ }
Vaka.captureException(err);
```

Docs: [SDK reference](https://vaka.hellsoft.se/sdk-node.html) ·
[install guide](https://vaka.hellsoft.se/install/node.html). License: MIT OR Apache-2.0.
