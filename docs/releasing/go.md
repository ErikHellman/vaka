# Releasing the Go SDK

The Go SDK is the module `github.com/ErikHellman/vaka/sdks/go`. Go has no package registry to
upload to: a module version is a git tag, and the public Go module proxy (`proxy.golang.org`)
downloads and caches it the first time someone asks for it. pkg.go.dev then lists it within
minutes. Because the module lives in a subdirectory, its tags are prefixed with the directory:
`sdks/go/v0.1.0`, not `v0.1.0` (those are the server's tags).

No account, token or secret is needed.

## Steps

1. Bump `Version` in `sdks/go/vaka.go` (it is sent as `sdk.version`) and note the change in the
   release notes. Merge to `main`.
2. Run the **Release Go SDK** workflow (Actions → Release Go SDK → Run workflow) on `main` with
   `version` set to the same value, for example `0.1.0`. It checks that the version matches
   `vaka.go`, runs the tests, pushes the tag `sdks/go/v0.1.0`, and asks the module proxy for it so
   the version is cached and `go get ...@latest` finds it right away.
3. Check `https://pkg.go.dev/github.com/ErikHellman/vaka/sdks/go` (it can take a few minutes) and
   install it in a scratch module:

   ```sh
   mkdir /tmp/try && cd /tmp/try && go mod init try
   go get github.com/ErikHellman/vaka/sdks/go@v0.1.0
   ```

## Doing it by hand

From a clone with push access to tags:

```sh
git tag -a sdks/go/v0.1.0 -m "Vaka Go SDK 0.1.0" origin/main
git push origin refs/tags/sdks/go/v0.1.0
GOPROXY=https://proxy.golang.org go list -m github.com/ErikHellman/vaka/sdks/go@v0.1.0
```

## Rules

- Tags are permanent. The proxy and the checksum database (`sum.golang.org`) remember the
  content of every version forever, so never move or re-push a tag: release a new patch version.
  To withdraw a broken version, add a `retract v0.1.1 // reason` line to `sdks/go/go.mod` and
  release the next patch version.
- Versions `v0.x` may break the API. From `v1.0.0` on, breaking changes need a new major version
  and module path (`github.com/ErikHellman/vaka/sdks/go/v2`).
- Keep the `go` directive in `go.mod` at the oldest Go release still supported upstream (the
  latest two), and the OpenTelemetry dependencies at versions that still support it, so users on
  older toolchains are not forced to upgrade. Users get newer OpenTelemetry versions through
  minimal version selection when their own module requires them.
- Pre-releases (`0.2.0-rc.1`) work the same way and are not picked by `@latest`.
