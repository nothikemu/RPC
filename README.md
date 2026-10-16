# DeskPresence

DeskPresence is a small Windows tray utility that shows the application you are using as your
**Discord Rich Presence**, for example *“Using Zed · 12:34 elapsed”*. It detects applications
automatically, lets you decide how each one appears (or whether it appears at all), and keeps
running quietly in the system tray.

It is built with Rust (all system work) and Tauri 2 + React (the settings window).

> **Status:** 0.1.0, first release candidate. The core logic is covered by automated tests on
> Linux and is type-checked for Windows. The Windows-specific parts (Win32 detection, icons,
> named pipes, tray, installer) build in CI on a Windows runner and must be verified on a real
> Windows 10/11 machine with the [manual QA checklist](docs/MANUAL_QA.md) before distribution.
> See [Known limitations](#known-limitations).

---

## Contents

- [Features](#features)
- [How it works](#how-it-works)
- [Setting up Discord](#setting-up-discord)
- [Installing](#installing)
- [Development](#development)
- [Testing](#testing)
- [Building the Windows installer](#building-the-windows-installer)
- [Configuration file](#configuration-file)
- [Privacy](#privacy)
- [Troubleshooting](#troubleshooting)
- [Known limitations](#known-limitations)
- [Screenshots](#screenshots)
- [License](#license)

## Features

**Detection and applications**
- Automatic detection of applications with visible, user-facing windows (the same kind of windows
  the taskbar shows), grouped per executable so multi-window and multi-process apps appear once.
- Debounced foreground tracking: brief popups and focus flicker don't switch the presence; clicking
  the taskbar, desktop or DeskPresence itself keeps the current app.
- Readable names from the executable's version information, with user overrides and fallbacks.
- Native executable icons, extracted once per executable version and cached on disk.
- Applications list with running state, “last seen” times, search and filters.

**Presence**
- Global *Details* and *State* templates with `{app}`, `{exe}`, `{elapsed}` and the opt-in
  `{window_title}` variable, a variable picker, inline validation and a live preview rendered by
  the Rust backend.
- Per-application overrides for text, display name, artwork (asset keys or `https://` URLs) and
  hover text.
- Timestamp modes: reset per app, keep counting across apps, since the app launched, or off.
- Activity type (Playing / Listening / Watching / Competing) and member-list status field.
- Unicode-safe truncation to Discord's limits with visible warnings.

**Selection rules**
- Foreground mode (default), priority mode, and manual pinning from the tray or the UI.
- Exclusions by exact path, executable name or wildcard pattern. Exclusions always win.
- Documented, tested precedence (see [Selection precedence](#selection-precedence)).

**System integration**
- Native tray icon with a live menu of detected applications. Click one to pin it, click it
  again to unpin. Also Pause, Configure… and Exit.
- Closing the window hides it to the tray (configurable). Single-instance protection.
- Optional launch at Windows sign-in (per-user, no administrator rights).
- Resilient Discord connection: reconnects with exponential backoff and jitter, handles Discord
  starting late, restarting or quitting, and clears your activity on exit.
- Versioned JSON configuration with atomic writes, backups, migrations, import (with a preview
  of what changes) and export.
- Diagnostics page, rotating local logs, and a sanitized diagnostics export.

## How it works

```
                      ┌──────────────────────────── deskpresence-core (Rust) ────────────────────────────┐
 Windows ──Win32──▶   │ app_detection ──snapshots──▶ runtime / presence_manager ──desired activity──▶   │ ──named pipe──▶ Discord
 (windows, processes) │  (own thread)    (watch)      rules · templates · pin       (watch)  discord_rpc │   \\?\pipe\discord-ipc-N
                      │ icon_cache (own thread) · config (atomic JSON) · diagnostics (events, redaction)  │
                      └──────────────────────────────────────┬───────────────────────────────────────────┘
                                                             │ RuntimeView (watch)
                      ┌────────────── src-tauri (Rust) ──────┴───────────────┐
                      │ tray (native menu) · commands (typed, validated)     │ ──events/commands──▶ React UI (src/)
                      │ startup (autostart) · logging · shutdown · window    │
                      └──────────────────────────────────────────────────────┘
```

| Path | Responsibility |
|------|----------------|
| `crates/deskpresence-core/src/app_detection/` | `WindowSystem` trait, platform-independent `Detector` (grouping, exit handling, metadata cache, debounced foreground), Win32 backend (`windows.rs`), mocks, detector thread |
| `crates/deskpresence-core/src/app_identity.rs` | `AppKey` (normalized executable path) and display-name resolution |
| `crates/deskpresence-core/src/icon_cache/` | Icon extraction (Win32), PNG validation, disk + memory cache, icon thread |
| `crates/deskpresence-core/src/rules.rs` | Matchers, exclusions, rule precedence |
| `crates/deskpresence-core/src/presence_templates.rs` | Template parser, rendering, UTF-16/grapheme-safe truncation |
| `crates/deskpresence-core/src/presence_manager.rs` | Selection policy, pin/pause, timers, activity construction |
| `crates/deskpresence-core/src/discord_rpc/` | IPC framing, payloads, transport discovery, connection lifecycle, backoff, rate limiting, mock server for tests |
| `crates/deskpresence-core/src/config/` | Schema, validation, migrations, atomic persistence, import diff |
| `crates/deskpresence-core/src/diagnostics.rs` | Event log, log throttling, redaction |
| `crates/deskpresence-core/src/runtime.rs` | Wires detector, engine and Discord service; produces the UI view |
| `crates/deskpresence-core/src/api.rs` | UI-facing types and the diagnostics bundle |
| `src-tauri/src/` | Tauri shell: `tray`, `commands`, `startup`, `logging`, `shutdown`, `window`, `state` |
| `src/` | React + TypeScript UI. `src/lib/bindings/` is generated from the Rust types by `ts-rs` |

Design notes:
- **Threads and ownership.** The detector owns the Win32 calls on its own thread and sleeps on a
  channel between ticks (250 ms foreground checks, full scans at the configured interval). The
  Discord connection is one async task. The engine task owns configuration, pin/pause state and
  events. All channels are bounded or latest-value (`watch`), so nothing can queue without limit,
  and no lock is held across I/O.
- **No duplicate work.** Snapshots are published only when something meaningful changes; the
  desired activity is compared before sending; the tray is rebuilt only when its app list changes.
  Icons are extracted once per executable version.
- **Untrusted input.** Every command re-validates its input in Rust. Icon requests only accept
  keys of known applications (the path comes from the detector or configuration, never the
  webview). Links, file dialogs and the log folder are opened by Rust with fixed targets. The
  webview's capability grants only DeskPresence's own commands and event listening.

### Selection precedence

1. **Paused** → no activity is sent.
2. **Exclusions** (exact path, executable name or pattern) always win: an excluded app is never
   shown, pinned or listed in the tray. **Disabled rules** also make an app ineligible.
3. **Pinned** app (tray or Applications page) wins while it runs; the pin clears automatically
   when the app exits or becomes excluded.
4. **Foreground mode:** the debounced foreground app. If the focused app is ineligible, either the
   most recently focused eligible app stays (`keep_previous`, default) or the activity clears
   (`clear`). With no app focused (desktop, taskbar, DeskPresence), the most recently focused
   eligible app is used.
5. **Priority mode:** among running, eligible apps *with an enabled rule*, the highest priority
   wins. Ties: currently focused → most recently focused → normalized path (alphabetical).
   Optionally falls back to the foreground policy when none is running.
6. Nothing eligible → the activity is **cleared**, never left stale.

Rule matching per app: an exact **path** rule beats an exact **executable-name** rule, which beats
a **pattern** (first matching pattern in list order wins). Display names never affect matching.

### Discord integration details

DeskPresence implements the documented *RPC over IPC* transport
([Discord RPC docs](https://discord.com/developers/docs/topics/rpc)) directly instead of using a
third-party crate: on Windows it connects to `\\?\pipe\discord-ipc-0` … `-9`, sends a
`HANDSHAKE` (`{"v":1,"client_id":"…"}`), waits for the `READY` dispatch, then sends `SET_ACTIVITY`
with its own process ID. Frames are an 8-byte little-endian header (opcode, length) plus JSON.
`PING` is answered with `PONG`; `CLOSE` and errors trigger a reconnect.

- **No secrets.** `SET_ACTIVITY` needs only the public Application ID — no OAuth flow, client
  secret or token exists in DeskPresence.
- **Rate limiting.** At most 5 updates per 20 seconds; faster changes coalesce into the latest
  activity (Discord does not document an exact limit for this command).
- **Field limits.** Text fields are capped at 128 characters (counted in UTF-16 units, cut on
  grapheme boundaries with `…`) and fields shorter than 2 characters are omitted. Discord's
  current docs do not list these limits; they match historical client validation and Discord's
  Game SDK.
- **Timestamps** are sent in Unix seconds, as in the `SET_ACTIVITY` example in Discord's docs.
- **What “connected” means.** Discord acknowledges each update from its app on this computer;
  DeskPresence shows that as “Accepted by the Discord client”. Discord provides no confirmation
  that the activity is visible to other people (privacy settings, “Display current activity”
  and Discord-side behavior are outside DeskPresence's control).
- **Rejected artwork.** If Discord rejects an update that includes artwork, DeskPresence retries
  once without artwork and shows a warning.

## Setting up Discord

Discord shows Rich Presence under a **Discord application**. Its name is the top line
(“Playing **DeskPresence**”) and its artwork can be shown. The **Windows application** you're
using appears in the Details/State text. These are two different identities:

1. Open the [Discord Developer Portal](https://discord.com/developers/applications) and create an
   application. Name it what you want people to see (e.g. “DeskPresence” or “Coding”).
2. Copy the **Application ID** from *General Information*.
3. Paste it into DeskPresence (first-run setup or **Settings → Discord**). DeskPresence checks
   the format (17–20 digits); Discord confirms it when connecting.
4. In Discord, *User Settings → Activity Privacy* must allow sharing your activity, otherwise
   others won't see it (the setting's exact wording varies between Discord versions).

**Artwork (optional).** In the Developer Portal, open *Rich Presence → Art Assets* and upload
images (Discord recommends 1024×1024). Use the asset **name** as the image key in DeskPresence
(globally under **Presence → Default artwork**, or per app in its rule). You can also use a public
`https://` image URL. DeskPresence cannot upload local executable icons to Discord; local icons
are only used inside DeskPresence.

**Build-time default ID.** Distributors can compile in a default public Application ID with
`DESKPRESENCE_DEFAULT_APP_ID=123456789012345678 npm run tauri build`. Users can still override it.
Without any ID, DeskPresence runs in preview mode and shows the presence only in its own window.

**Distribution note.** Rich Presence via local RPC works for any user with the Discord desktop
client. Discord's RPC documentation describes tester and approval requirements for the
*authenticated* RPC commands (guilds, voice, etc.); DeskPresence does not use those commands.
Review Discord's Developer Terms and policies before distributing an application under your
Discord application's name.

## Installing

Download `DeskPresence_<version>_x64-setup.exe` from a CI run or release and run it. The
installer:

- installs **per user** (no administrator rights) and appears in *Settings → Apps → Installed
  apps*;
- requires **Microsoft Edge WebView2**, which ships with Windows 11 and current Windows 10. If it
  is missing, the installer downloads the WebView2 bootstrapper (needs internet once);
- keeps your settings on upgrade. The uninstaller removes the startup entry; it deletes your
  settings and cache only if you tick *Delete the application data*.

Supported: Windows 10 and Windows 11, x64. The plain `deskpresence.exe` is not a
verified portable build — use the installer.

## Development

**Prerequisites**
- Windows 10/11 x64 with the [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/):
  Microsoft C++ Build Tools (MSVC, Windows SDK) and WebView2.
- Rust stable 1.90 or newer (`rustup`), Node.js 22.

```powershell
npm install
npm run tauri dev          # runs the Vite dev server and the app with hot reload
```

Without Windows, you can work on the UI in a browser: `npm run dev` and open
`http://localhost:1420`. The UI then uses a clearly labelled **browser preview** backend with
sample data. On Linux, `DESKPRESENCE_MOCK_DETECTOR=1 npm run tauri dev` runs the real Rust
backend with labelled mock applications (Linux builds are for development only).

Regenerate the TypeScript bindings after changing UI-facing Rust types:

```bash
npm run bindings           # cargo test -p deskpresence-core --features ts export_bindings
```

## Testing

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
npm run typecheck
npm run lint
npm test
npm run build
```

What the automated tests cover:

- **Core (Rust, ~150 tests):** identity normalization; rule matching and precedence; exclusion
  precedence; priority ordering and tie-breaking; pin/unpin and pin clearing on exit; template
  rendering, unknown variables and escapes; Unicode-safe truncation (emoji ZWJ sequences,
  combining marks, astral characters); configuration validation, normalization, migration,
  atomic persistence, corrupt/partial/newer files, backups and import limits; activity payload
  construction; IPC frame encoding/decoding and malformed frames (unknown opcode, oversized,
  truncated, invalid JSON); connection state transitions, backoff timing, invalid IDs,
  Discord restarts, timeouts, PING/PONG, rejected artwork, rate limiting, duplicate suppression,
  clean shutdown; detector grouping, deduplication, exit handling, PID lookups failing mid-scan,
  metadata caching and title privacy; icon-cache keys, corrupt cache files and negative caching;
  end-to-end runtime tests (mock window system → engine → mock Discord server).
- **Real transports:** a full session over a real Unix socket (Linux) and a real Windows named
  pipe (Windows CI), using unique pipe names so a running Discord is never contacted.
- **Win32 (Windows only):** own-process identity, version metadata and icon extraction from
  `explorer.exe`, window enumeration with title privacy.
- **App shell:** tray model (labels, truncation, escaping, overflow, in-place updates), log
  rotation and tail, launch arguments, command input validation.
- **Frontend (Vitest):** template and validation helpers, template input behavior, Settings
  validation/save/discard/reset confirmation, and the full onboarding flow with and without an
  Application ID.

The Discord tests run against an in-process mock IPC server (`discord_rpc/mock.rs`) with Tokio's
paused clock, so reconnect and backoff behavior is deterministic and needs no Discord client.

## Building the Windows installer

On Windows:

```powershell
npm ci
npm run tauri build
```

Artifacts:

- Installer: `target\release\bundle\nsis\DeskPresence_0.1.0_x64-setup.exe`
- Executable: `target\release\deskpresence.exe`

The CI workflow (`.github/workflows/ci.yml`) runs all checks on `windows-latest` and uploads
the installer as the `deskpresence-windows-installer` artifact. Code signing and auto-update are
not configured; unsigned installers trigger SmartScreen warnings. See
[docs/RELEASING.md](docs/RELEASING.md).

Branding lives in `src-tauri/tauri.conf.json` (`productName`, `identifier`, icons),
`crates/deskpresence-core/src/lib.rs` (`PRODUCT_NAME`), and `src-tauri/icons/source/*.svg`
(regenerate with `npx tauri icon src-tauri/icons/source/app-icon.svg -o src-tauri/icons`).
Changing `identifier` moves the configuration folder.

## Configuration file

`%APPDATA%\com.deskpresence.desktop\config.json` (logs: `%LOCALAPPDATA%\com.deskpresence.desktop\logs`,
icon cache: `%LOCALAPPDATA%\com.deskpresence.desktop\icons`).

- Versioned (`schema_version`). Older layouts are migrated after a backup
  (`config.backup-v0-<time>.json`). Files from a **newer** version are never overwritten silently:
  DeskPresence backs them up and starts with defaults.
- Writes go to a temporary file that is flushed and renamed over the old one, so a crash cannot
  leave a half-written file. Unchanged settings are not rewritten.
- An unreadable file is backed up (`config.backup-invalid-<time>.json`), defaults are used and a
  notice explains what happened.
- Rules are identified by stable IDs; display names are presentation only.
- Import validates the file (size limit, JSON, schema, every field) and shows the list of changes
  before replacing anything. A backup is made before import and reset.

## Privacy

DeskPresence has **no telemetry, accounts, cloud sync or network catalog**. It does not inject
into processes, read application contents, launch applications or require administrator rights.

- **Read locally:** visible windows (handle, owning process), executable paths, version metadata
  and icons. Window **titles** are read only if you enable *Settings → Privacy → Allow window
  titles*; they are never written to disk or logs.
- **Stored locally:** your configuration, an icon cache, and rotating logs (≈8 MB maximum)
  containing application names and paths but never window titles.
- **Sent to Discord (the app on this computer):** only the activity you configure — Details and
  State text, artwork keys/URLs, hover text, timestamp and activity type — plus DeskPresence's
  process ID. Discord then publishes it on your profile. Nothing is sent while paused or when
  nothing is eligible.
- **Diagnostics export:** user-folder names in paths, rendered presence text and your Discord
  user name are removed unless you tick *Include full paths…*.

Use **Pause** (tray or window) to stop sharing temporarily and **Exit** to quit completely.

## Troubleshooting

| Symptom | What to try |
|---------|-------------|
| “Discord not running” | Start the Discord **desktop** app (the browser version has no local connection). DeskPresence retries automatically (1 s → 60 s). **Diagnostics → Reconnect** retries now. |
| “ID rejected” | The Application ID doesn't belong to an existing Discord application. Copy it again from the Developer Portal. |
| Connected, but friends don't see the activity | In Discord's *Activity Privacy* settings, allow sharing your activity. Discord may also hide activity in some contexts; DeskPresence can only confirm the local client accepted it. |
| Artwork missing | The asset key must match an uploaded asset of *your* Discord application (newly uploaded assets can take a few minutes to appear), or be an `https://` URL. |
| An app is missing from the list | Only apps with normal, visible windows are detected. Minimized Store (UWP) apps can disappear while suspended. Tool/overlay windows and Windows shell surfaces are ignored by design. Apps running as administrator are detected, but their window titles may be unavailable. |
| Wrong app shown | Check **Rules** (mode, exclusions, priorities) and **Diagnostics → Presence source**. Increase *Focus change delay* if quick popups steal the presence. |
| The tray icon is hidden | Drag it from the taskbar's “^” overflow area. |
| Settings were reset | See the notice on Overview: the previous file was saved next to `config.json`. |
| Anything else | **Diagnostics → Export diagnostics** and **Open log folder**. Set *Log detail* to “Detailed” to capture more. |

## Known limitations

- **Windows verification pending.** The Win32 code is compiled, linted and unit tested in CI on
  Windows, but tray behavior, focus tracking with real applications, startup registration and
  the installer must be checked with [docs/MANUAL_QA.md](docs/MANUAL_QA.md) before release.
- Applications whose windows are cloaked (other virtual desktops, suspended Store apps) are not
  listed while cloaked.
- Elevated (administrator) apps: identity is read with limited rights, which works for most but
  not all protected processes; unidentifiable windows are skipped.
- `{elapsed}` in text updates once per minute (to respect rate limits); Discord's own timer
  (timestamp) is live.
- The experimental “use the detected app's name as the activity name” option relies on a
  documented activity field that Discord may ignore for local integrations.
- Discord field-length limits are not documented by Discord today; DeskPresence uses the
  historical 128-character limit.
- No code signing, auto-update, or ARM64 build yet.
- Linux/macOS builds exist only for development; there is no supported non-Windows release.

## Screenshots

No screenshots are committed: the only environment used so far had no Windows desktop, and
screenshots with sample data would misrepresent the product. To capture real ones, install the
app on Windows 11, open a few applications, and use **Win + Shift + S** on the Overview,
Applications and Presence pages (light and dark themes) and on the tray menu.

## License

DeskPresence is licensed under the [MIT License](LICENSE).

Main dependencies and their licenses: Tauri and its official plugins (autostart, dialog,
opener, single-instance), wry and tao — MIT OR Apache-2.0; `windows` crate — MIT OR Apache-2.0;
webview2-com — MIT; Tokio — MIT; serde/serde_json — MIT OR Apache-2.0; tracing and
tracing-subscriber — MIT; png, base64, fastrand, thiserror, unicode-segmentation — MIT OR
Apache-2.0; ts-rs (development only) — MIT; auto-launch — MIT; React — MIT; Lucide icons — ISC;
Vite — MIT. Microsoft Edge WebView2 Runtime is distributed by Microsoft under its own terms.
“Discord” is a trademark of Discord Inc.; DeskPresence is not affiliated with Discord.
