# Install Vaka in a Windows desktop app

Project: `{{PROJECT}}` · Endpoint: `{{ENDPOINT}}` · Ingest key: `{{KEY}}`

These steps are written to be followed by a coding agent. Do them in order and verify at the end.
The ingest key is write-only and safe to ship inside the installer.

Vaka has no separate "desktop SDK": a Windows app uses the SDK for the language it is written in.
This guide picks the right one and adds the desktop-specific steps.

## 1. Identify the app's stack

Look at the repository and pick the first row that matches:

| The app is built with | Use | Full guide (`get_install_instructions` platform) |
|---|---|---|
| .NET (WPF, WinForms, WinUI 3, MAUI, Avalonia, Uno) | `Vaka` NuGet package (no host) | `dotnet` |
| Electron (`electron` in `package.json`) | `@hellsoft/vaka-node` in the main process + `@hellsoft/vaka-browser` in renderers | `node` and `javascript` |
| Tauri (`src-tauri/`) | `vaka-sdk` crate in the Rust core + `@hellsoft/vaka-browser` in the webview | `rust` and `javascript` |
| Rust (egui, iced, Slint, windows-rs, ...) | `vaka-sdk` crate | `rust` |
| Kotlin / Java (Compose Multiplatform Desktop, JavaFX, Swing) | `se.hellsoft.vaka:vaka-jvm` | `jvm` |
| Python (PyQt, PySide, Tkinter) | `vaka-sdk` from PyPI | `python` |
| Go (Fyne, Wails) | `github.com/ErikHellman/vaka/sdks/go` | `go` |
| C / C++ / Win32 / MFC / Qt / anything else | HTTP ingest API | `http` |

Fetch the full guide for that row and follow it: over MCP call
`get_install_instructions(project="{{PROJECT}}", platform="<platform>")` (key and endpoint are filled in),
or read `{{ENDPOINT}}/docs/install/<platform>.md` and substitute the values above.
Then come back here for steps 2 to 4.

## 2. Desktop specifics

- **Release**: use the version shown in the app's About window and file properties (assembly
  `Version`, the `package.json` version for Electron, `tauri.conf.json` version for Tauri). Use the
  same string for symbol uploads in step 3 and for `vaka releases` commands.
- **Environment**: `debug` for development builds, `production` for signed installer builds, so
  local crashes do not pollute release health.
- **Sessions**: one session per app launch. Start the SDK before the first window opens and dispose
  it on exit (`Application.Exit`/`OnExit` in WPF, `Application.ApplicationExit` in WinForms, Electron
  `before-quit`, keep the Rust guard alive until `main` returns) so crash-free rates are right.
- **.NET UI thread exceptions**: WPF and WinForms catch UI-thread exceptions before they reach the
  process-wide handler. Report them from `DispatcherUnhandledException` (WPF) or
  `Application.ThreadException` (WinForms) with the SDK's capture call, then let the app decide
  whether to keep running.
- **Electron renderers**: allow `{{ENDPOINT}}` in the Content-Security-Policy `connect-src`.
  `@hellsoft/vaka-node` needs Node.js 22.15+, so check `process.versions.node` for the Electron
  version in use.
- **Proxies**: corporate networks often require a proxy. The SDKs use the platform HTTP stack, so
  check that the app's HTTP client honours the system proxy if events do not arrive.
- **Privacy**: analytics are anonymous by default. Only call `identify` if the app has signed-in
  users and its privacy policy allows it.

## 3. Upload symbols for release builds

Native crashes are symbolicated on the server with the PDB of the same build, matched by debug id
(GUID + age).

- Rust / C / C++: build release with PDBs (Rust MSVC does by default) and run
  `vaka upload debug-files path\to\app.pdb` for each shipped binary.
- .NET: keep the portable PDB next to the assembly (the default for `dotnet publish`).
- Electron / Tauri webviews: upload source maps for the renderer bundle as described in the
  `javascript` guide.

Set `VAKA_URL={{ENDPOINT}}`, `VAKA_PROJECT={{PROJECT}}` and `VAKA_TOKEN` (a read-write API token,
kept in CI secrets, never in the app) before running the CLI.

## 4. Verify

Build and launch the app, trigger a test error from a debug menu or a temporary button, then check
that it arrived: `list_issues(project="{{PROJECT}}", since="1h")` over MCP, or
`VAKA_URL={{ENDPOINT}} VAKA_KEY={{KEY}} vaka send-test-event` to check connectivity alone.
Confirm the event shows `os` = Windows. Remove the temporary trigger afterwards.
