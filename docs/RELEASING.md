# Releasing DeskPresence

## Version

Update the version in all three places (they must match):

- `Cargo.toml` → `[workspace.package] version`
- `src-tauri/tauri.conf.json` → `version`
- `package.json` → `version`

If the configuration format changes incompatibly, bump `CURRENT_SCHEMA_VERSION` in
`crates/deskpresence-core/src/config/schema.rs` and add a migration step in
`config/migrate.rs` (with a test). Older files are migrated after a backup; newer files are
never overwritten silently.

## Build

On Windows 10/11 x64 with the Tauri prerequisites (MSVC build tools, WebView2):

```powershell
npm ci
npm run typecheck; npm run lint; npm test
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
npm run tauri build
```

Optional default Discord Application ID (public, not a secret):

```powershell
$env:DESKPRESENCE_DEFAULT_APP_ID = "123456789012345678"; npm run tauri build
```

Outputs:

| Artifact | Path |
|----------|------|
| NSIS installer | `target\release\bundle\nsis\DeskPresence_<version>_x64-setup.exe` |
| Application executable | `target\release\deskpresence.exe` |

CI (`.github/workflows/ci.yml`) produces the same installer on every push to `main` and on pull
requests, uploaded as the `deskpresence-windows-installer` artifact. Set the repository variable
`DESKPRESENCE_DEFAULT_APP_ID` to compile in a default ID.

## Before publishing

1. Run [MANUAL_QA.md](MANUAL_QA.md) on Windows 10 and Windows 11 with the CI-built installer.
2. Measure idle CPU/memory as described there and update the README if needed.
3. Optionally sign the installer and executable (Authenticode). Without signing, Windows
   SmartScreen warns on first run. Tauri's signing options are in its
   [Windows signing guide](https://tauri.app/distribute/sign/windows/).

## Notes

- The installer is per-user (`installMode: currentUser`) and needs no administrator rights.
- WebView2: `webviewInstallMode` is `downloadBootstrapper`, so the installer downloads WebView2
  only if it is missing (needs internet once). For offline installs, switch to
  `embedBootstrapper` or `offlineInstaller` in `tauri.conf.json` (larger installer).
- `src-tauri/windows/installer-hooks.nsh` removes the `Run` registry value on uninstall. If you
  change `productName`, update the value name there as well (the autostart plugin names the
  value after the product).
- Changing `identifier` in `tauri.conf.json` moves the configuration and log folders.
- Cross-compiling from Linux with `cargo build --target x86_64-pc-windows-gnu` is useful as a
  link check only; that build loads `WebView2Loader.dll` dynamically. Ship MSVC builds.
