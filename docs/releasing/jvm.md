# Releasing the JVM SDK to Maven Central

The JVM SDK (`sdks/jvm`) is published as `se.hellsoft.vaka:vaka-jvm` to Maven Central through the
[Sonatype Central Portal](https://central.sonatype.com), using the
[`com.vanniktech.maven.publish`](https://vanniktech.github.io/gradle-maven-publish-plugin/central/)
Gradle plugin. The workflow `.github/workflows/release-jvm.yml` does the upload; the steps below are
the one-time setup and the per-release checklist.

The Android SDK (`se.hellsoft.vaka:vaka-android`) uses the same namespace, so the account, namespace
verification and signing key below can be reused for it.

## One-time setup

### 1. Central Portal account and namespace

1. Sign in at <https://central.sonatype.com> (GitHub login works).
2. **Namespaces → Add namespace** `se.hellsoft`. Covering `se.hellsoft` also covers
   `se.hellsoft.vaka`.
3. Verify it with a DNS TXT record on `hellsoft.se`: the portal shows a verification key; add
   `TXT hellsoft.se "<verification key>"` at the DNS provider, wait for it to resolve
   (`dig +short TXT hellsoft.se`), then press **Verify**. The record can be removed afterwards.

   (Alternative without DNS: use the namespace `io.github.erikhellman`, which the portal verifies
   from the GitHub login, and change `GROUP` in `sdks/jvm/gradle.properties` plus the coordinates in
   the docs.)

### 2. Portal user token

**Account → Generate User Token**. This gives a username and password pair (not your login). Keep
both; they become the `mavenCentralUsername` and `mavenCentralPassword` secrets below.

### 3. GPG signing key

Maven Central requires every file to be signed.

```sh
gpg --quick-generate-key "Erik Hellman <you@example.com>" rsa4096 sign never
gpg --list-secret-keys --keyid-format short          # note the key id, e.g. ABCD1234
gpg --keyserver keyserver.ubuntu.com --send-keys ABCD1234
gpg --keyserver keys.openpgp.org --send-keys ABCD1234
gpg --armor --export-secret-keys ABCD1234 > signing-key.asc
```

The public key must be on a key server so Central can check the signatures. Store
`signing-key.asc` and its passphrase in a password manager, then delete the file.

### 4. GitHub secrets

In the repository: **Settings → Secrets and variables → Actions → New repository secret**:

| Secret | Value |
| --- | --- |
| `ORG_GRADLE_PROJECT_mavenCentralUsername` | user token username (step 2) |
| `ORG_GRADLE_PROJECT_mavenCentralPassword` | user token password (step 2) |
| `ORG_GRADLE_PROJECT_signingInMemoryKey` | the full contents of `signing-key.asc`, including the `BEGIN`/`END` lines |
| `ORG_GRADLE_PROJECT_signingInMemoryKeyId` | optional: the short key id |
| `ORG_GRADLE_PROJECT_signingInMemoryKeyPassword` | the key's passphrase |

These are the same names the [Android guide](android.md) uses, so one set serves both SDKs. The
workflow passes them to Gradle as environment variables of the same names.

## Each release

1. Set `VERSION_NAME` in `sdks/jvm/gradle.properties` (for example `0.3.0`) and update the version
   in `docs/install/jvm.md`. Merge to `main`. Versions on Maven Central are immutable: a published
   version can never be replaced or deleted.
2. **Actions → Release JVM SDK → Run workflow** on `main`:
   - `version`: the same value as `VERSION_NAME` (the run fails if they differ, as a typo guard).
   - `mode`: `release` uploads and publishes; `upload` uploads to the portal and leaves the
     deployment for you to inspect and publish (or drop) under **Deployments**; `dry-run` builds,
     tests and publishes to the runner's local Maven repository only (no secrets needed).
   Pushing a tag `jvm-vX.Y.Z` runs the same workflow in `release` mode.
3. The deployment is validated by Central (signatures, POM, sources and javadoc jars) and is usually
   searchable on <https://central.sonatype.com/artifact/se.hellsoft.vaka/vaka-jvm> within 30 minutes,
   and resolvable from `repo1.maven.org` shortly after.

## Releasing from a laptop

With the same values in `~/.gradle/gradle.properties`:

```properties
mavenCentralUsername=...
mavenCentralPassword=...
signingInMemoryKey=-----BEGIN PGP PRIVATE KEY BLOCK-----\n...\n-----END PGP PRIVATE KEY BLOCK-----
signingInMemoryKeyPassword=...
```

(the key on one line with `\n` for line breaks), run:

```sh
cd sdks/jvm
./gradlew :vaka-jvm:publishAndReleaseToMavenCentral
```

## Troubleshooting

- **401 from the portal**: the user token is wrong or expired; generate a new one (step 2).
- **"Namespace not verified"**: the group in `gradle.properties` is not under a verified namespace.
- **Signature validation failed**: the public key is not on a key server yet, or the secret is
  missing the armor lines.
- **Check the build without publishing**: `./gradlew :vaka-jvm:publishToMavenLocal` and look in
  `~/.m2/repository/se/hellsoft/vaka/vaka-jvm/`.
