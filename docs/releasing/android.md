# Releasing the Android SDK and Gradle plugin

Two artifacts, both under the group `se.hellsoft.vaka`:

| Artifact | Source | Consumers use |
| --- | --- | --- |
| `se.hellsoft.vaka:vaka-android` (AAR + sources) | `sdks/android/vaka-android` | `implementation("se.hellsoft.vaka:vaka-android:X.Y.Z")` |
| Gradle plugin `se.hellsoft.vaka` (jar + plugin marker `se.hellsoft.vaka.gradle.plugin`) | `sdks/android/vaka-gradle-plugin` | `id("se.hellsoft.vaka") version "X.Y.Z"` |

Both go to **Maven Central** through the Central Portal (central.sonatype.com). The install
guide tells apps to resolve plugins from `mavenCentral()`, so the plugin marker goes to Central
too; publishing it to the Gradle Plugin Portal as well is optional.

The builds already apply `maven-publish` with a POM (name, description, URL, licenses), and
`publishToMavenLocal` works. What Central additionally needs: a verified namespace, signed
artifacts, javadoc jars, and POM `scm` and `developers` sections. The one-time setup below adds
those with the [vanniktech maven-publish plugin](https://vanniktech.github.io/gradle-maven-publish-plugin/),
which uploads to the Central Portal.

## One-time setup

1. **Namespace.** Sign in at central.sonatype.com, add the namespace `se.hellsoft.vaka` (or
   `se.hellsoft`), and verify it with the DNS TXT record it shows for `hellsoft.se`.
2. **Portal token.** Account → Generate User Token. Keep the username/password pair.
3. **Signing key.** `gpg --full-generate-key` (RSA 4096 or ed25519), then publish the public
   key: `gpg --keyserver keyserver.ubuntu.com --send-keys <KEY_ID>`. Export the private key for
   CI: `gpg --armor --export-secret-keys <KEY_ID>`.
4. **Credentials**, in `~/.gradle/gradle.properties` (never in the repo):

   ```properties
   mavenCentralUsername=<portal token username>
   mavenCentralPassword=<portal token password>
   signingInMemoryKey=<the armored private key, newlines as \n>
   signingInMemoryKeyId=<last 8 hex digits of the key id>
   signingInMemoryKeyPassword=<key passphrase>
   ```

   For CI, the same values as repository secrets `ORG_GRADLE_PROJECT_mavenCentralUsername`,
   `ORG_GRADLE_PROJECT_mavenCentralPassword`, `ORG_GRADLE_PROJECT_signingInMemoryKey`,
   `ORG_GRADLE_PROJECT_signingInMemoryKeyId` and `ORG_GRADLE_PROJECT_signingInMemoryKeyPassword`.
5. **Build changes** (one commit):
   - In `sdks/android/build.gradle.kts` add
     `id("com.vanniktech.maven.publish") version "0.34.0" apply false`.
   - In `vaka-android/build.gradle.kts`, replace `maven-publish`, the `android.publishing { }`
     block and the `publishing { }` block with:

     ```kotlin
     plugins { id("com.vanniktech.maven.publish") }

     mavenPublishing {
         publishToMavenCentral(automaticRelease = false)
         signAllPublications()
         coordinates("se.hellsoft.vaka", "vaka-android", sdkVersion)
         pom {
             name.set("Vaka Android SDK")
             description.set("Crash reporting, release health and privacy-friendly analytics for Android.")
             url.set("https://github.com/ErikHellman/vaka")
             licenses {
                 license { name.set("MIT"); url.set("https://opensource.org/licenses/MIT") }
                 license { name.set("Apache-2.0"); url.set("https://www.apache.org/licenses/LICENSE-2.0") }
             }
             developers { developer { id.set("ErikHellman"); name.set("Erik Hellman") } }
             scm {
                 url.set("https://github.com/ErikHellman/vaka")
                 connection.set("scm:git:https://github.com/ErikHellman/vaka.git")
             }
         }
     }
     ```

   - In `vaka-gradle-plugin/build.gradle.kts`, the same plugin and block with
     `coordinates("se.hellsoft.vaka", "vaka-gradle-plugin", version.toString())`; it detects
     `java-gradle-plugin` and publishes the plugin marker as well. Read the version from the
     root `gradle.properties` (`VERSION_NAME`) instead of the hard-coded `version = "X.Y.Z"`, so
     one bump covers both.
   - Check with `./gradlew :vaka-android:publishToMavenLocal` and
     `./gradlew -p vaka-gradle-plugin publishToMavenLocal`, then look in
     `~/.m2/repository/se/hellsoft/vaka/` for `.asc` signatures, `-sources.jar` and `-javadoc.jar`.

## Publish a release

From the release tag, in `sdks/android`:

```sh
git checkout v0.3.0
cd sdks/android
./gradlew :vaka-android:testDebugUnitTest
./gradlew :vaka-android:publishToMavenCentral
./gradlew -p vaka-gradle-plugin publishToMavenCentral
```

With `automaticRelease = false` the upload waits as a deployment in the Central Portal
(Publishing → Deployments): check that it validated, then press **Publish**. Use
`publishAndReleaseToMavenCentral` to skip that step once releases are routine. Artifacts
appear on `repo1.maven.org` within about 30 minutes; search.maven.org takes longer.

Verify from a clean project, or with:

```sh
curl -sI https://repo1.maven.org/maven2/se/hellsoft/vaka/vaka-android/0.3.0/vaka-android-0.3.0.pom | head -1
curl -sI https://repo1.maven.org/maven2/se/hellsoft/vaka/se.hellsoft.vaka.gradle.plugin/0.3.0/se.hellsoft.vaka.gradle.plugin-0.3.0.pom | head -1
```

Maven Central releases are permanent: a version can't be replaced or deleted. Fix a bad
release with a new patch version.

## Optional: Gradle Plugin Portal

Apps that only list `gradlePluginPortal()` need the plugin there too. Add
`id("com.gradle.plugin-publish") version "1.3.1"` to the plugin build, set `website` and
`vcsUrl` in `gradlePlugin { }`, put `gradle.publish.key` and `gradle.publish.secret` (from
plugins.gradle.org → API keys) in `~/.gradle/gradle.properties`, and run
`./gradlew -p vaka-gradle-plugin publishPlugins`. The first version of a plugin id is reviewed
by Gradle before it shows up.

## After the first publish

Remove the "not on Maven Central yet" notes from the release notes, and keep the
"building from source" fallback in `docs/install/android.md` as it is.
