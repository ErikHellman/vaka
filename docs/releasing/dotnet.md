# Releasing the .NET SDK to NuGet

The three packages (`Vaka`, `Vaka.OpenTelemetry`, `Vaka.AspNetCore`) are always released together
with the same version, by the [`release-dotnet`](../../.github/workflows/release-dotnet.yml) workflow.

## One-time setup (owner)

1. Sign in at <https://www.nuget.org> with a Microsoft account. Optionally create an organization
   (for example `hellsoft`) so more people can publish later.
2. Reserve the package IDs (optional but recommended): request ID-prefix reservation for `Vaka.`
   from your account settings, so nobody else can publish `Vaka.*` packages.
3. Give the workflow a way to publish. Pick one:

   **Trusted publishing (recommended, no long-lived secret).** On nuget.org go to your user menu →
   *Trusted Publishing* → *Add*, and enter repository owner `ErikHellman`, repository `vaka`,
   workflow file `release-dotnet.yml`. Then in GitHub → *Settings* → *Secrets and variables* →
   *Actions* → *Variables*, add `NUGET_USER` = your nuget.org user name (not your email).

   **API key.** On nuget.org → *API Keys* → *Create*: scope *Push new packages and package
   versions*, glob pattern `Vaka*`, expiry up to 365 days. In GitHub → *Settings* → *Secrets and
   variables* → *Actions* → *Secrets*, add `NUGET_API_KEY` with the key. Renew it before it expires.

   If both are set, trusted publishing is used.

## Releasing a version

1. Make sure `main` is green and `docs/sdk-dotnet.md` describes the release.
2. GitHub → *Actions* → *Release .NET SDK* → *Run workflow*, on `main`, with `version` such as `0.2.0`
   (no `v`). Pre-releases work too: `0.2.0-beta.1`.

   Alternatively push a tag `dotnet-v0.2.0`; the version is taken from the tag.
3. The workflow runs the tests on .NET 8 and 10, packs the three packages and their symbol packages
   (`.snupkg`, with Source Link to the commit), pushes them to nuget.org (`--skip-duplicate`, so a re-run
   is safe), uploads them as a workflow artifact, and tags the commit `dotnet-vX.Y.Z` when started by hand.
4. nuget.org validates and indexes new packages in 5–15 minutes. Check
   <https://www.nuget.org/packages/Vaka.AspNetCore>, then `dotnet add package Vaka.AspNetCore --version X.Y.Z`
   in a scratch project.
5. Keep `<Version>` in `sdks/dotnet/Directory.Build.props` in step with the other SDKs (see [README.md](README.md)); it is only the
   default for local builds; the workflow always passes the version explicitly).

A published version can't be deleted or replaced, only *unlisted* (nuget.org → package → *Manage* →
*Listing*). Fix forward with a new version.

## Building packages locally

```sh
cd sdks/dotnet
dotnet pack -c Release -p:Version=0.2.0 -o out
dotnet nuget push "out/*.nupkg" --source https://api.nuget.org/v3/index.json --api-key "$NUGET_API_KEY"
```

`dotnet nuget push` uploads the matching `.snupkg` symbol packages automatically.
