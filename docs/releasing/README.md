# Releasing the SDKs

How to publish each Vaka SDK to its package registry. The server, Docker image and CLI are
released by [`release.yml`](../../.github/workflows/release.yml) (see
[docs/releases](../releases)); SDKs are published separately, from the same tag.

| SDK | Registry | Guide |
| --- | --- | --- |
| `@hellsoft/vaka-browser`, `@hellsoft/vaka-bundler-plugin`, `@hellsoft/vaka-node` | npm | [npm.md](npm.md) |
| `se.hellsoft.vaka:vaka-android` + Gradle plugin `se.hellsoft.vaka` | Maven Central, Gradle Plugin Portal | [android.md](android.md) |
| `Vaka` (iOS, macOS, tvOS) | Swift Package Manager (git tags), CocoaPods trunk | [ios.md](ios.md) |
| `vaka-sdk` (Rust) | crates.io | [rust.md](rust.md) |
| `github.com/ErikHellman/vaka/sdks/go` | Go module proxy (git tags) | [go.md](go.md) |
| `se.hellsoft.vaka:vaka-jvm` | Maven Central | [jvm.md](jvm.md) |
| `vaka-sdk` (Python) | PyPI (trusted publishing) | [python.md](python.md) |
| `Vaka`, `Vaka.OpenTelemetry`, `Vaka.AspNetCore` (.NET) | NuGet | [dotnet.md](dotnet.md) |

## Release order

1. Bump every version (see "Versions" below) and add `docs/releases/vX.Y.Z.md`; push to `main`.
2. Wait for CI to pass, then dispatch **Release** with `tag=vX.Y.Z`. It creates the `vX.Y.Z`
   tag and the bare `X.Y.Z` alias that Swift Package Manager needs.
3. Publish each SDK from that tag with the guides above. Order doesn't matter; none of them
   depend on each other.
4. Update the install guides if a package was published for the first time (they currently say
   where a package is not on a registry yet).

## Versions

All SDKs share the server's version. Change all of these together:

| Where | Field |
| --- | --- |
| `Cargo.toml` | `[workspace.package] version` (server, CLI, Rust SDK) |
| `sdks/js/package.json`, `sdks/js/src/index.ts` | `version`, `SDK.version` |
| `sdks/js-bundler/package.json` | `version` |
| `sdks/node/package.json`, `sdks/node/src/index.ts` | `version`, `SDK.version` |
| `sdks/python/src/vaka/_version.py` | `__version__` |
| `sdks/go/vaka.go` | `Version` (the Release Go SDK workflow checks it) |
| `sdks/android/gradle.properties` | `VERSION_NAME` |
| `sdks/android/vaka-gradle-plugin/build.gradle.kts` | `version` |
| `sdks/jvm/gradle.properties`, `docs/install/jvm.md` | `VERSION_NAME`, version in the examples |
| `sdks/ios/Sources/Vaka/Vaka.swift` | `sdkVersion` |
| `sdks/dotnet/Directory.Build.props` | `Version` (default for local builds; the NuGet workflow passes the version explicitly) |
| `Package.swift`, `docs/install/ios.md`, `docs/install/android.md` | Version numbers in the examples |
| `Cargo.lock`, `sdks/js/package-lock.json`, `sdks/node/package-lock.json` | Regenerate (`cargo update -w`, `npm install`); rebuild `sdks/js/dist/vaka.min.js` |

`grep -rn '"0.4.0"\|= "0.4.0"\|VERSION_NAME=' --include=*.json --include=*.ts --include=*.toml --include=*.kts --include=*.properties --include=*.swift --include=*.py .`
(with the old version) finds them.

## Credentials

Registry credentials belong to the maintainer, not the repository. Where a GitHub workflow
publishes, the token is a repository secret (Settings → Secrets and variables → Actions); the
guides name each one. Never commit tokens, signing keys or `~/.gradle/gradle.properties`.
