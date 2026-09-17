# Contributing to DroidBridge

Thanks for your interest! Bug reports, device reports and pull requests are all welcome.

## Reporting bugs

Open an issue with the bug report template. Device facts matter a lot for this project: model,
Android version, ROM, and whether you use root (Magisk), Shizuku or neither. The in-app
**Settings → Diagnostics → Export diagnostics** file is the most useful attachment; check it before
sharing and remove anything you consider private.

If DroidBridge was stopped in the background, run this before rebooting and attach the output:

```bash
adb shell dumpsys activity exit-info com.droidbridge.android
```

## Project layout

| Path | What lives there |
|---|---|
| `app/` | Android app (Kotlin, Jetpack Compose), the `:runtime` service process and device tests |
| `rust/crates/runtime` | The Runtime core and the MCP facade shared by every host |
| `rust/crates/app_native` | JNI library for the App-hosted Runtime and the ChatGPT tunnel client |
| `rust/crates/daemon`, `supervisor` | The root daemon and its supervisor shipped in the Magisk module |
| `rust/crates/contract`, `domain`, `persistence` | Public contract, admission rules and the canonical store |
| `magisk/` | Magisk module scripts and the framework helper sources |
| `tools/` | Release tooling, notice generation and toolchain checks |

## Building

The toolchain is pinned. On Windows with PowerShell 7:

1. Install JDK 17, the Android SDK (platform 37, Build Tools 36.0.0, NDK 29.0.14206865,
   CMake 3.31.6), Rust 1.98.0 and `cargo-ndk` 4.1.2.
2. Set `sdk.dir` in `local.properties`.
3. Build libpcap once: `pwsh tools/build-libpcap.ps1`.
4. Check the toolchain: `pwsh tools/check-toolchain.ps1`.
5. Build: `./gradlew :app:assembleDebug :app:assembleDebugMagiskModule`.

The debug build installs as `com.droidbridge.android.debug` next to a release install, and its
Magisk module is `droidbridge_debug`.

## Before sending a pull request

```bash
./gradlew :app:testDebugUnitTest :app:lintDebug
cd rust
cargo test --locked --workspace
cargo ndk -t arm64-v8a --platform 33 clippy --locked -p daemon -p app_native --all-targets --features daemon/debug-module -- -D warnings
```

- Keep changes focused, and match the style of the surrounding code.
- Add or update tests with behavior changes. Device-only behavior belongs in `app/src/androidTest`.
- Never commit secrets, API keys, tunnel IDs, keystores or device dumps.
- User-visible strings go in both `values/strings.xml` and `values-zh-rCN/strings.xml`.

By contributing you agree that your contribution is licensed under the Apache License 2.0.
