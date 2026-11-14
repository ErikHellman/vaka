# Deploy on Fly.io

Vaka runs on [Fly.io](https://fly.io) as two apps: the server, public over HTTPS, and ClickHouse,
private and reachable only over Fly's internal network. Metadata stays in SQLite on a volume.
The setup costs roughly a shared-cpu-1x and a shared-cpu-2x machine plus two small volumes.

## Settings

`fly.toml` has no variable interpolation, so the repository ships templates
(`deploy/fly/vaka.fly.toml.tmpl` and `deploy/fly/clickhouse/fly.toml.tmpl`) and a script,
`deploy/fly/fly.sh`, that fills them in. It reads `deploy/fly/fly.env` (git-ignored; start from
`deploy/fly/fly.env.example`), and variables already set in your shell win over the file.
`VAKA_FLY_ENV=path/to/file` points it at another file, which is handy for several instances.

| Variable | Default | Meaning |
| --- | --- | --- |
| `VAKA_FLY_APP` | required | Server app name. Fly app names are global, so pick your own |
| `VAKA_FLY_CLICKHOUSE_APP` | `<VAKA_FLY_APP>-clickhouse` | Private ClickHouse app; the server reaches it at `<name>.internal:8123` |
| `VAKA_FLY_REGION` | `arn` | Region for both apps and volumes (`fly platform regions`) |
| `VAKA_PUBLIC_URL` | `https://<VAKA_FLY_APP>.fly.dev` | Public URL used in links, alerts and install guides |
| `VAKA_FLY_ORG` | your personal org | Organization the apps are created in |
| `VAKA_FLY_IMAGE` | build from source | Deploy a published image instead, e.g. `ghcr.io/erikhellman/vaka:0.3.0` |
| `VAKA_FLY_VM_SIZE`, `VAKA_FLY_VM_MEMORY` | `shared-cpu-1x`, `512mb` | Server machine |
| `VAKA_FLY_VOLUME_GB` | `3` | Server volume (SQLite and debug files) |
| `VAKA_FLY_CLICKHOUSE_VM_SIZE`, `VAKA_FLY_CLICKHOUSE_VM_MEMORY` | `shared-cpu-2x`, `2gb` | ClickHouse machine |
| `VAKA_FLY_CLICKHOUSE_VOLUME_GB` | `10` | ClickHouse volume |
| `VAKA_ADMIN_EMAIL`, `VAKA_ADMIN_PASSWORD` | | First owner account, set as secrets by `launch`. A password is generated and printed when empty |

Any other [server setting](self-hosting.md#configuration) goes in as a Fly secret, for example
`fly secrets set -a <app> VAKA_RESEND_API_KEY=re_...`.

## First deploy

Install [flyctl](https://fly.io/docs/flyctl/install/) and run `fly auth login`, then:

```sh
git clone https://github.com/ErikHellman/vaka && cd vaka
cp deploy/fly/fly.env.example deploy/fly/fly.env   # set VAKA_FLY_APP and VAKA_ADMIN_EMAIL
deploy/fly/fly.sh config    # check the resolved settings
deploy/fly/fly.sh launch
```

`launch` renders `fly.toml` and `deploy/fly/clickhouse/fly.toml`, creates both apps and their
volumes, generates a shared ClickHouse password and sets it as a secret on both apps, sets the admin
account, and deploys ClickHouse and then the server. Every step is skipped when it is already done,
so it is safe to run again. Open `<VAKA_PUBLIC_URL>/ui` when it finishes.

Both apps run a single machine each because volumes are per machine; the script deploys with
`--ha=false` so Fly does not add a second one. Fly snapshots volumes daily.

## Redeploys and upgrades

```sh
git pull
deploy/fly/fly.sh deploy            # both apps
deploy/fly/fly.sh deploy server     # only the server (the usual upgrade)
deploy/fly/fly.sh render            # only write the fly.toml files, e.g. to run fly commands by hand
```

Don't edit the generated `fly.toml` files: change `fly.env` or the templates and render again.

## Custom domain

```sh
fly certs add vaka.example.com -a <VAKA_FLY_APP>
```

Add the DNS records it prints, set `VAKA_PUBLIC_URL=https://vaka.example.com` in `fly.env`, and run
`deploy/fly/fly.sh deploy server`.

## Example: vaka.fly.dev

The project's own instance at `https://vaka.fly.dev` uses only `VAKA_FLY_APP=vaka`; every other
setting is the default (ClickHouse app `vaka-clickhouse`, region `arn`, public URL
`https://vaka.fly.dev`):

```sh
VAKA_FLY_APP=vaka deploy/fly/fly.sh deploy server
```
