# Privacy and data retention

Vaka is built so that analytics need no cookie banner and crash reports carry as little
personal data as possible. Defaults are strict; identification is opt-in.

## Web analytics

- No cookies, no localStorage, no fingerprinting beyond what is described here.
- The browser SDK keeps a random session id in `sessionStorage` (one tab, cleared when the tab
  closes, rotated after 30 idle minutes) to group one visit's pageviews, events and errors. It is
  never shared across tabs, sites or days. Turn it off with `sessions: false`.
- Visitors are counted with `hash(daily salt + project + IP + User-Agent)`. The salt rotates
  every day and old salts are deleted, so the same person cannot be linked across days and the
  hash cannot be reversed.
- IP addresses are used for the hash and the country lookup, then discarded. They are never stored.
- Stored per pageview: URL path and hostname, referrer, UTM tags, browser, OS, device type,
  country, and any custom properties you send.

## Mobile analytics

- The SDKs generate a random install id (`anon_id`), stored in app storage. Vaka hashes it with a
  per-project key before storage. Reinstalling the app creates a new id.
- No advertising ids, device ids or IP addresses are stored.

## User identification (opt-in)

Nothing links data to a person unless the app calls `identify(userId)`. Use an internal,
opaque id rather than an email address. `identify(null)` stops sending it.

Identified users can be listed (`GET /api/v1/projects/{p}/users`) and their activity read as a
timeline, by anyone with read access to the project. Anonymous visitors never appear there.

## Crash report scrubbing

Before storage, error events are scrubbed (per project, on by default):

- Values of keys such as `password`, `secret`, `token`, `api_key`, `access_token`,
  `authorization`, `cookie`, `session`, `credit_card`, `cvv`, `ssn`, `private_key` are replaced
  with `[Filtered]`. Add your own keys with `scrub_fields` on the project.
- Email addresses, card-like numbers, bearer/basic credentials, and secrets in URL query strings
  are masked in messages, breadcrumbs, tags and extra data.

SDKs also offer a `beforeSend`-style hook to drop or edit data on the device.

## Retention

Each project has a retention period (default 90 days, `VAKA_DEFAULT_RETENTION_DAYS`). Errors,
events and sessions older than that are deleted automatically. Change it with
`PATCH /api/v1/projects/{p}` `{"retention_days": 30}`; lowering it deletes older data within an hour.

## Deleting a user's data

To honour a deletion request (GDPR article 17), delete everything stored for a user id:

```sh
curl -X DELETE -H "Authorization: Bearer $VAKA_TOKEN" {{ENDPOINT}}/api/v1/projects/web/users/user-123
```

or use the MCP tool `delete_user_data`. This removes the user's errors, events and sessions and
is recorded in the audit log. Data sent without `identify` is not linked to a user id and cannot be
looked up per person.

## Where data lives

Everything stays on your server: ClickHouse for events, SQLite or Postgres for metadata, and the
data directory for uploaded debug files. Vaka sends nothing to third parties, except email
through Resend if you configure it.
