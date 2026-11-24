# Install Vaka in a Linux desktop app

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.
The ingest key is write-only and safe to ship inside the package.

Vaka has no separate "desktop SDK": a Linux app uses the SDK for the language it is written in.
This guide picks the right one and adds the desktop-specific steps.

## 1. Identify the app's stack

Look at the repository and pick the first row that matches:

| The app is built with | Use | Full guide (`get_install_instructions` platform) |
|---|---|---|
| Electron (`electron` in `package.json`) | `@hellsoft/vaka-node` in the main process + `@hellsoft/vaka-browser` in renderers | `node` and `javascript` |
| Tauri (`src-tauri/`) | `vaka-sdk` crate in the Rust core + `@hellsoft/vaka-browser` in the webview | `rust` and `javascript` |
| Rust (GTK-rs, egui, iced, Slint, ...) | `vaka-sdk` crate | `rust` |
| .NET (Avalonia, Uno) | `Vaka` NuGet package (no host) | `dotnet` |
| Kotlin / Java (Compose Multiplatform Desktop, JavaFX, Swing) | `se.hellsoft.vaka:vaka-jvm` | `jvm` |
| Python (PyGObject/GTK, PyQt, PySide, Tkinter) | `vaka-sdk` from PyPI | `python` |
| Go (Fyne, Wails, gotk) | `github.com/ErikHellman/vaka/sdks/go` | `go` |
| C / C++ / GTK / Qt / anything else | HTTP ingest API | `http` |

Fetch the full guide for that row and follow it: over MCP call
`get_install_instructions(project="{{PROJECT}}", platform="<platform>")` (key and endpoint are filled in),
or read `{{ENDPOINT}}/docs/install/<platform>.md` and substitute the values above.
Then come back here for steps 2 to 4.

## 2. Desktop specifics

- **Release**: use the version the package is published with (`.deb`/`.rpm`, Flatpak, Snap or
  AppImage version). Use the same string for symbol uploads in step 3 and for `vaka releases` commands.
- **Environment**: `debug` for development builds, `production` for packaged builds, so local
  crashes do not pollute release health.
- **Sessions**: one session per app launch. Start the SDK before the first window opens and let
  it shut down cleanly on quit (keep the Rust guard alive until `main` returns, flush in the
  toolkit's quit handler, Electron `before-quit`) so crash-free rates are right.
- **Sandboxes**: Flatpak apps need `--share=network` in `finish-args`; Snaps need the `network`
  plug. Without it the SDK cannot reach `{{ENDPOINT}}`.
- **Electron renderers**: allow `{{ENDPOINT}}` in the Content-Security-Policy `connect-src`.
  `@hellsoft/vaka-node` needs Node.js 22.15+, so check `process.versions.node` for the Electron
  version in use.
- **Privacy**: analytics are anonymous by default. Only call `identify` if the app has signed-in
  users and its privacy policy allows it.

## 3. Upload symbols for release builds

Native crashes are symbolicated on the server with the debug files of the same build, matched by
ELF build id.

- Rust / C / C++: keep debug info before stripping (for example `objcopy --only-keep-debug`, or
  Rust `debug = "line-tables-only"`) and run `vaka upload debug-files path/to/binary-or.debug`.
- .NET: keep the portable PDB next to the assembly (the default).
- Electron / Tauri webviews: upload source maps for the renderer bundle as described in the
  `javascript` guide.

Set `VAKA_URL={{ENDPOINT}}`, `VAKA_PROJECT={{PROJECT}}` and `VAKA_TOKEN` (a read-write API token,
kept in CI secrets, never in the app) before running the CLI.

## 4. Verify

Build and launch the app, trigger a test error from a debug menu or a temporary button, then check
that it arrived: `list_issues(project="{{PROJECT}}", since="1h")` over MCP, or
`VAKA_URL={{ENDPOINT}} VAKA_KEY={{KEY}} vaka send-test-event` to check connectivity alone.
Confirm the event shows `os` = Linux. Remove the temporary trigger afterwards.
