# Quickstart

## 1. Run Vaka

```sh
git clone https://github.com/ErikHellman/vaka && cd vaka
cp .env.example .env      # set VAKA_ADMIN_EMAIL, VAKA_ADMIN_PASSWORD and VAKA_PUBLIC_URL
docker compose up -d
curl http://localhost:8080/health    # {"ok":true,...}
```

This starts Vaka and ClickHouse, with metadata in SQLite. See [self-hosting](self-hosting.md)
for Postgres, TLS and other options.

## 2. Create an API token

Sign in at `http://localhost:8080/ui` with the admin account from `.env`, open **Tokens** and
create a token. Pick **write** and **all projects** for an agent that will set up projects.
Tokens start with `vt_` and are shown once.

## 3. Connect your coding agent

Add Vaka as an MCP server (Streamable HTTP) with the token as a bearer token. For Claude Code:

```sh
claude mcp add --transport http vaka http://localhost:8080/mcp \
  --header "Authorization: Bearer vt_..."
```

Then ask the agent: *"Create a Vaka project for this app and install the SDK."* It uses the
`create_project` and `get_install_instructions` tools and follows the install guide.

## 4. Or do it by hand

```sh
export VAKA_URL=http://localhost:8080 VAKA_TOKEN=vt_...
curl -s -X POST $VAKA_URL/api/v1/projects -H "Authorization: Bearer $VAKA_TOKEN" \
  -H 'Content-Type: application/json' -d '{"name": "My web app", "platform": "javascript"}'
```

The response has the project's `ingest_key` (`vk_...`) and an `install_instructions_url` that
returns Markdown with the key and endpoint filled in. Follow it, or see the SDK pages:
[JavaScript](sdk-javascript.md), [Android](sdk-android.md), [iOS](sdk-ios.md), [Rust](sdk-rust.md),
[Node.js](sdk-node.md), [HTTP](sdk-http.md).

## 5. Check that data arrives

```sh
vaka send-test-event --key vk_...       # or: VAKA_KEY=vk_... vaka send-test-event
vaka issues -p my-web-app
```

Install the CLI with `cargo install --git https://github.com/ErikHellman/vaka vaka`, or use the
binary inside the Docker image: `docker compose exec vaka vaka --help`.
