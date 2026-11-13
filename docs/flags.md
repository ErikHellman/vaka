# Feature flags and experiments

Flags live in Vaka next to the crashes and analytics they affect, so an agent can see that a
variant crashes more or converts better, and roll it back with one call.

## Flags

A flag has a `key`, an `enabled` switch, a `rollout` percentage (default 100), optional
`variants` with weights, targeting `rules` and per-environment switches:

```sh
curl -X POST {{ENDPOINT}}/api/v1/projects/web/flags -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{
    "key": "new_checkout", "rollout": 50,
    "variants": [{"key": "control", "weight": 1}, {"key": "one_page", "weight": 1}],
    "rules": [{"user_ids": ["qa@example.com"], "variant": "one_page"}, {"properties": {"plan": "enterprise"}, "rollout": 0}],
    "environments": {"staging": true}
  }'
```

Evaluation, in order:

1. The environment switch (environments not listed follow `enabled`). Off: the flag is `false`.
2. The first rule whose `user_ids` (if any) contains the person and whose `properties` (if any) all
   match (`"!value"` means not equal). A rule with `variant` returns it; a rule with `rollout`
   replaces the flag's rollout.
3. Rollout: the person is in if `bucket < rollout / 100`, where `bucket` is the first 53 bits of
   `sha256(key + ":" + distinct_id)` as a number in [0, 1). The same person always gets the same
   answer, and raising the rollout only adds people.
4. Variants split the rolled-out people by weight. Without variants the flag is `true`.

## Evaluating in apps

`POST /ingest/v1/flags` with the ingest key:

```json
{"distinct_id": "device-or-user-id", "user_id": "optional", "environment": "production", "properties": {"plan": "pro"}}
```

returns `{"flags": {"new_checkout": "one_page", "dark_mode": true, "beta": false}, "details": {...}}`.

The JavaScript SDK does this with `flags: true` (see [sdk-javascript](sdk-javascript.md)):
`getFlag(key)`, `isEnabled(key)`, `onFlags(cb)`, `loadFlags(properties)`. Other SDKs can call the
endpoint and attach the values.

**Exposure**: send the values you used as `flags` (`{"new_checkout": "one_page", "beta": "false"}`)
on every error, session and event item. All health and conversion numbers are split by these
values, so include `false` for flags that are off.

## Health by variant

`GET /projects/{p}/flags/{key}/health` (MCP `flag_health`): per variant, sessions, crash-free and
healthy sessions, errors per session, plus issues whose events come mostly from one variant
(`lift` = share of the issue's events / share of sessions). To roll back, `update_flag` with
`rollout: 0`, `enabled: false`, or a rule `{"variant": "control"}`.

## Experiments

An experiment is a flag plus a goal event (step syntax):

```sh
curl -X POST {{ENDPOINT}}/api/v1/projects/web/experiments -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{"name": "one page checkout", "flag": "new_checkout", "goal": "purchase", "control": "control"}'
curl -H "Authorization: Bearer $VAKA_TOKEN" "{{ENDPOINT}}/api/v1/projects/web/experiments/one%20page%20checkout/results"
```

Results per variant: `exposed` (people with any event carrying the flag), `converted` (of those,
people who did the goal while the flag was set), `conversion_rate`, `uplift` against the control,
a 95% interval for the difference, the p-value of a two-sided two-proportion z-test, and a
`verdict`. A result counts as significant with p < 0.05, at least 30 exposed people per variant
and 10 conversions. Without a saved experiment use `GET /projects/{p}/flags/{key}/results?goal=signup`.

| REST | MCP |
| --- | --- |
| `GET/POST /projects/{p}/flags` | `list_flags`, `create_flag` |
| `GET/PATCH/DELETE /projects/{p}/flags/{key}` | `get_flag`, `update_flag`, `delete_flag` |
| `GET /projects/{p}/flags/{key}/health` | `flag_health` |
| `GET /projects/{p}/flags/{key}/results` | `experiment_results` with `flag` and `goal` |
| `GET/POST /projects/{p}/experiments` | `list_experiments`, `create_experiment` |
| `GET /projects/{p}/experiments/{name}/results` | `experiment_results` with `experiment` |
