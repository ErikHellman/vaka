# Vaka Android SDK

Crash reporting, release health and privacy-friendly analytics for Android apps.

- `vaka-android/`: the library (`se.hellsoft.vaka:vaka-android`).
- `vaka-gradle-plugin/`: Gradle plugin `se.hellsoft.vaka` that uploads R8 mapping files.
- `sample/`: a small app using both.

Docs: [SDK reference](../../docs/sdk-android.md) · [install guide](../../docs/install/android.md).

## Development

Needs JDK 17+, an Android SDK and the NDK + CMake 3.22.1 for `libvaka.so` (the native crash
handler; Gradle installs them if licenses are accepted). Point Gradle at the SDK with `sdk.dir` in `local.properties` or `ANDROID_HOME`).

```sh
./gradlew :vaka-android:testDebugUnitTest          # unit tests
vaka-android/src/test/cpp/run.sh                    # native crash handler, built for the desktop (Linux)
./gradlew :vaka-android:publishToMavenLocal         # library to ~/.m2
./gradlew -p vaka-gradle-plugin publishToMavenLocal # plugin to ~/.m2

# Send real data to a running server (otherwise this test is skipped):
VAKA_TEST_ENDPOINT=http://localhost:8080 VAKA_TEST_KEY=vk_... \
  ./gradlew :vaka-android:testDebugUnitTest --tests '*LiveServerTest*'

# Minified sample build that uploads its mapping:
VAKA_URL=http://localhost:8080 VAKA_PROJECT=my-app VAKA_TOKEN=vt_... ./gradlew :sample:assembleRelease
```
