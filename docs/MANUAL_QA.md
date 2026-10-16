# Manual QA checklist (Windows 10 / 11)

Run this on a real Windows 10 or Windows 11 x64 machine before a release. Automated tests cover
the logic, protocol and Win32 calls; this checklist covers what only a real desktop session can
show (tray, focus behavior with real applications, startup, installer, Discord client).

**Setup**
- A Discord account signed in to the **Discord desktop app** (stable). A second account or a
  friend to confirm the profile from another device is helpful.
- A Discord application in the [Developer Portal](https://discord.com/developers/applications)
  with at least one uploaded art asset (e.g. key `logo`).
- A few ordinary applications: a browser, a code editor, Notepad, File Explorer, a Microsoft
  Store app (e.g. Calculator), a game launcher if available.
- The installer from CI (`deskpresence-windows-installer` artifact).

Record the Windows version (`winver`), Discord version and DeskPresence version in the report.

## 1. Install and first run

- [ ] The installer runs **without an administrator prompt** (UAC) and finishes.
- [ ] DeskPresence appears in *Settings → Apps → Installed apps* with name, version and icon.
- [ ] First launch shows the setup window. All six steps render in light and dark Windows themes.
- [ ] Entering `abc` as the Application ID shows a digits-only error; a 17–20 digit ID is accepted.
- [ ] *Open Developer Portal* opens the browser at the Developer Portal.
- [ ] Finishing setup shows the **Ready** screen with the Discord status and detected-app count.
- [ ] Closing the window leaves the **tray icon** visible (check the “^” overflow area).

## 2. Detection

- [ ] **Applications** lists the open apps with their real icons and readable names
  (e.g. “Visual Studio Code”, not “Code”), one row per app even with several windows.
- [ ] Notepad, File Explorer and a Store app (Calculator) are detected; the taskbar, desktop,
  Start menu, Search and DeskPresence itself are **not** listed.
- [ ] Opening a new app adds it within about a second; closing it moves it to “last seen”.
- [ ] Switching between apps changes *Showing* within about half a second.
- [ ] Alt+Tab quickly through several apps: the presence does **not** flicker through each.
- [ ] Clicking the taskbar, the desktop or the tray icon does **not** change the shown app.
- [ ] An app running **as administrator** (e.g. an elevated terminal) is either listed or
  skipped — DeskPresence keeps working either way and nothing crashes.
- [ ] Minimizing all windows keeps the most recently used app shown.

## 3. Discord Rich Presence

- [ ] With Discord running and a valid Application ID, Overview shows **Connected** and
  “Accepted by the Discord client”.
- [ ] Your Discord profile (view it from another account/device) shows the Discord application's
  name, the *Details* text (“Using …”) and an elapsed timer.
- [ ] Changing the *Details* template to `Working in {app}` and saving updates the profile.
- [ ] `{bogus}` in a template shows an inline error and cannot be saved.
- [ ] Setting *Default artwork → Large image* to your asset key shows the image on the profile.
  A non-existent key keeps the text visible (DeskPresence may warn).
- [ ] *Activity type → Watching* changes the verb on the profile.
- [ ] Timestamp *Reset when the app changes*: the timer restarts after switching apps;
  *Keep counting across apps*: it does not.
- [ ] **Quit Discord** (tray → Quit Discord): Overview changes to *Discord not running* within a few
  seconds; DeskPresence keeps running; CPU stays idle (Task Manager).
- [ ] **Start Discord again**: DeskPresence reconnects on its own (within about a minute) and the
  presence reappears.
- [ ] **Restart Discord** while connected: same recovery, Diagnostics shows a reconnect.
- [ ] Start DeskPresence **before** Discord: it connects once Discord starts.
- [ ] Invalid but well-formed Application ID (e.g. `12345678901234567`): Overview shows
  *ID rejected*; DeskPresence does not retry rapidly (Diagnostics: next retry in ~5 minutes).
- [ ] No Application ID: Overview shows *Not set up* and a preview only; nothing appears on Discord.

## 4. Tray

- [ ] Right-click the tray icon: status line, the running eligible apps (real names, long names
  truncated with “…”), *Pause presence*, *Configure…*, *Exit*.
- [ ] An app name containing `&` displays the ampersand correctly.
- [ ] Click an app: it gets a check mark and becomes the presence (*Pinned*). Click it again:
  check mark removed, automatic selection resumes.
- [ ] Close the pinned app: the pin clears automatically and Diagnostics records it.
- [ ] Opening/closing apps while the menu is **closed** updates the list next time it opens; keep
  the menu **open** while an unrelated app's state changes — the menu does not close or jump.
- [ ] *Pause presence*: the profile clears; the icon turns grey; *Pause* is checked. Unpause restores.
- [ ] Left-click the tray icon (and *Configure…*) opens and focuses the window, also when it was
  minimized or behind other windows.
- [ ] *Exit*: the profile activity disappears immediately, the tray icon disappears, and
  `deskpresence.exe` is gone from Task Manager within a few seconds.

## 5. Rules

- [ ] Exclude an app from **Applications**: focusing it keeps the previous app (default) and it is
  gone from the tray menu. With *Clear the presence* selected, focusing it clears the profile.
- [ ] Disable an app's rule: it is not shown automatically and not offered in the tray.
- [ ] Priority mode with two rules (priorities 10 and 5): the priority-10 app is shown while both
  run, regardless of focus; closing it switches to the other.
- [ ] A pattern exclusion such as `*helper*.exe` hides matching apps.
- [ ] Rename an app (display name): the rule still matches after restarting DeskPresence.

## 6. Settings, startup and persistence

- [ ] Enable *Launch at Windows startup*; Task Manager → *Startup apps* lists DeskPresence.
  Sign out and in: DeskPresence starts **in the tray** without showing the window.
- [ ] Disable it: the Task Manager entry disappears (or shows disabled).
- [ ] *Start minimized*: launching from the Start menu starts in the tray.
- [ ] Turn *Keep running when this window is closed* off: closing the window exits completely
  (activity cleared).
- [ ] Launch DeskPresence a **second time** from the Start menu: no second tray icon; the existing
  window comes to the front.
- [ ] Theme: System/Light/Dark switch immediately.
- [ ] All settings survive a restart (`%APPDATA%\com.deskpresence.desktop\config.json`).
- [ ] Export settings, change something, Import the file: the dialog lists the differences before
  replacing; after import a `config.backup-before-import-*.json` exists.
- [ ] Reset settings asks for confirmation and creates a backup.
- [ ] Corrupt `config.json` by hand (e.g. delete the last `}`) and restart: DeskPresence starts with
  defaults, Overview explains what happened, and a `config.backup-invalid-*.json` exists.
- [ ] Window-title tracking: off by default; `{window_title}` stays empty until enabled; titles
  never appear in `%LOCALAPPDATA%\com.deskpresence.desktop\logs\deskpresence.log`.

## 7. Diagnostics and resources

- [ ] Diagnostics shows connection state, last attempt, last activity update, detector status,
  scan duration and recent events (repeated events grouped).
- [ ] *Open log folder* opens Explorer at the log folder.
- [ ] *Export diagnostics* (default): the JSON contains no user-folder names, no presence text and
  no Discord user name. With *Include full paths…* checked, they are present.
- [ ] Idle for 10 minutes with the window closed: record CPU (should stay near 0 %), memory
  (`deskpresence.exe` + `msedgewebview2.exe` processes) and handle count in Task Manager, and
  confirm the handle count does not keep growing.

## 8. Upgrade and uninstall

- [ ] Install a newer build over the old one: settings and rules are kept.
- [ ] Uninstall without *Delete the application data*: the startup entry is removed; the
  configuration folder remains.
- [ ] Uninstall with it ticked: `%APPDATA%\com.deskpresence.desktop` and
  `%LOCALAPPDATA%\com.deskpresence.desktop` are removed.
