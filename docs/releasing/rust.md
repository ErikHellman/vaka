# Releasing the Rust SDK

Crate `vaka-sdk` (`crates/vaka-sdk-rust`). Its version comes from `[workspace.package]` in the
root `Cargo.toml`, shared with the server and the CLI, so it is bumped with every release.

Only the SDK goes to crates.io. The server (`vaka-server`) and CLI (`vaka`) ship as the Docker
image and the release binaries, and have `publish = false` so they are never published by accident.

## One-time setup

1. Sign in to crates.io with GitHub, verify an email address, and create an API token with the
   `publish-new` and `publish-update` scopes, limited to `vaka-sdk`
   (Account Settings → API Tokens).
2. `cargo login` with it on the machine you publish from, or store it as the `CARGO_REGISTRY_TOKEN`
   repository secret. Better: after the first publish, set up **trusted publishing** on the
   crate's settings page (GitHub repository `ErikHellman/vaka`, your workflow file name) and use
   `rust-lang/crates-io-auth-action` in that workflow instead of a stored token.
3. Crate metadata (`description`, `license`, `repository`, `readme`, `keywords`, `categories`)
   and the crate page (`crates/vaka-sdk-rust/README.md`) are in place. The server and CLI have
   `publish = false`.

## Publish a release

```sh
git checkout v0.3.0
cargo test -p vaka-sdk --all-features
cargo publish -p vaka-sdk --dry-run     # builds the packaged crate in isolation
cargo publish -p vaka-sdk
```

`--dry-run` catches files missing from the package and dependencies without a version (every
dependency must come from crates.io; path or git dependencies are refused). Check
https://crates.io/crates/vaka-sdk and, a few minutes later, the docs build at
https://docs.rs/vaka-sdk.

## Mistakes

A published version can't be overwritten or deleted. `cargo yank -p vaka-sdk --version 0.3.0`
stops new projects from resolving it (existing lockfiles keep working); then publish a patch.

## After a minor version bump

The first publish was 0.2.0 (2026-11-14). When the minor version changes, update the version in
`docs/install/rust.md`, `docs/sdk-rust.md` and `crates/vaka-sdk-rust/README.md`.
