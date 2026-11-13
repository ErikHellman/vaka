# Releasing the iOS SDK

The Swift package is the repository itself: the root `Package.swift` exposes the `Vaka`
library from `sdks/ios/Sources`. Apps add it with

```swift
.package(url: "https://github.com/ErikHellman/vaka", from: "0.3.0")
```

## Swift Package Manager (every release)

SwiftPM resolves versions from git tags in plain semver (`0.3.0`, not `v0.3.0`). The **Release**
workflow creates both `vX.Y.Z` and the bare `X.Y.Z` alias, so there is nothing to publish:

1. Bump `sdkVersion` in `sdks/ios/Sources/Vaka/Vaka.swift` with the other versions.
2. Dispatch **Release** with `tag=vX.Y.Z` (see [README.md](README.md)).
3. Check: `git ls-remote --tags https://github.com/ErikHellman/vaka | grep -E 'refs/tags/X\.Y\.Z$'`.
4. Verify on a Mac: in a scratch package, depend on `from: "X.Y.Z"` and run `swift build`
   (or add the package in Xcode via File → Add Package Dependencies).

The Swift Package Index (swiftpackageindex.com) picks up new tags by itself once the package
has been added there once (Add a Package → the repository URL).

Never move or delete a published tag: SwiftPM caches the commit hash per version and builds
fail with a fingerprint mismatch. Release a new patch version instead.

## CocoaPods (optional)

The CocoaPods team has announced that the trunk becomes read-only (planned for December 2026),
after which no new pods or versions can be pushed. Check https://blog.cocoapods.org first; SwiftPM
is the supported channel. While trunk still accepts pushes, apps not on SwiftPM can get the SDK
this way:

1. One-time: `pod trunk register you@example.com 'Your Name'` and click the email link.
2. Add `Vaka.podspec` at the repository root:

   ```ruby
   Pod::Spec.new do |s|
     s.name         = 'Vaka'
     s.version      = '0.3.0'
     s.summary      = 'Crash reporting, release health and analytics for Apple platforms.'
     s.homepage     = 'https://github.com/ErikHellman/vaka'
     s.license      = { :type => 'MIT OR Apache-2.0', :file => 'LICENSE-MIT' }
     s.author       = 'Erik Hellman'
     s.source       = { :git => 'https://github.com/ErikHellman/vaka.git', :tag => s.version.to_s }
     s.swift_version = '5.9'
     s.ios.deployment_target  = '15.0'
     s.osx.deployment_target  = '12.0'
     s.tvos.deployment_target = '15.0'
     s.source_files = 'sdks/ios/Sources/Vaka/**/*.swift', 'sdks/ios/Sources/VakaNative/**/*.{c,h}'
     s.public_header_files = 'sdks/ios/Sources/VakaNative/include/*.h'
     s.libraries    = 'z'
   end
   ```

   The C target `VakaNative` becomes part of the same pod; check that the Swift sources
   `import VakaNative` still compile (`pod lib lint` tells you). If they don't, split it into a
   `Vaka/Native` subspec with `module_name = 'VakaNative'`.
3. Each release, after the tag exists: bump `s.version`, then
   `pod spec lint Vaka.podspec` and `pod trunk push Vaka.podspec`.

## dSYM upload is not part of the SDK release

Apps upload their own dSYMs with the `vaka` CLI (see `docs/install/ios.md`); the SDK ships as
source, so there is nothing to symbolicate for it.
