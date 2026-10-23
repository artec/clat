# CLAT desktop client

CLAT's desktop app is the native macOS/Windows client of the same local Host used by
CLAT's attached TUI and web workbench. The installer contains its own private
backend: installing a separate `clat` command is optional. The CLI remains a
single-file installation with no Tauri or user-side Node/Python requirement.

The installed display name is **CLAT**: `CLAT.app` on macOS, and CLAT in the
Windows Start Menu and shortcuts. The desktop executable remains `clat-desktop`
(`clat-desktop.exe` on Windows), with application identifier
`com.artec.clat.desktop`. The separate CLI executable remains `clat`; desktop
release assets keep the `clat-desktop-` prefix.

## Install and open a project

Desktop installers ship alongside CLI archives in each desktop-enabled formal
[GitHub release](https://github.com/artec/clat/releases/latest):

- macOS 13+: choose `macos-arm64.dmg` or `macos-x64.dmg`, then drag CLAT
  into Applications.
- Windows 10 22H2 x64 / Windows 11 ARM64: choose the matching `windows-*-setup.exe`.
  Installation is per-user. The installer bootstraps shared Evergreen WebView2
  when needed; it does not ship a browser runtime or modify your CLI PATH.

From v2.1.2, each macOS architecture has three files: the DMG for installation,
an App archive for automatic updates, and a combined SHA-256 list. Windows has
one installer/update EXE and its checksum. All signatures live in the shared
`latest.json`. Users of v2.1.1 or earlier must manually install v2.1.2 once;
subsequent releases use the new built-in update path.

Choose **打开项目** (Open project), select a directory and explicitly approve
project trust when requested. Model configuration, permissions, tools, sessions
and tasks use the existing workbench and the Host's normal policies.
Windows has native window controls and no application menu bar. Desktop actions
live in the page's **桌面** panel. macOS retains standard system editing/window
menus. Completion and pending approval/question notifications contain no
conversation text; clicking one opens its project and admitted session.
The current Windows notification plugin only reports clicks while the banner is
on screen. Historical Action Center clicks and cold-start activation are not
supported yet; restore the app through its tray when the banner has expired.

## Coexistence and background work

Desktop, CLI/TUI and PWA share the current storage root, normally `~/.clat`.
There is one Host/writer, not a second desktop database. Each project's active
session remains shared across its clients. Desktop uses one main window.

From the v2.1.2 coexistence baseline, official CLI/Desktop versions need not match:
authenticated Host/wire/journal contracts and native-client capabilities decide
whether they can share the running Host. Development builds retain strict
executable identity checks. Installed CLI/Desktop versions and the running Host
version are separate; replacing an installed executable does not itself replace
its running process.

A newer packaged backend uses the Host's existing atomic idle takeover during
activation. It never downgrades a newer Host. Missing/incompatible
Hosts are reported explicitly; reconnecting does not silently start another one.
From v2.2.0, automatic discovery and subsequent refresh use one core coordinator:

- A compatible Host is connected directly. During initial activation, a newer
  packaged backend can take over when idle; a busy compatible Host remains usable.
  Once attached, Desktop keeps that Host for this interaction, even if it later
  becomes idle. A newer backend can activate on the next Desktop launch or explicit
  project open. This avoids reloading page-local unsent text merely for a newer
  executable.
- Equal/newer compatible Hosts are reused; an older backend never downgrades one.
  Incompatible protocols, unavailable projects, missing trust and failed takeover
  acknowledgements surface errors instead of forcing a stop or starting a second
  writer. Development backends require explicit startup for mismatched builds.
- Missing/disconnected Hosts require **打开项目** to start. Switching Hosts can
  briefly disconnect other clients; refresh the PWA or reopen a current TUI if
  needed. Existing tasks must finish before automatic takeover is accepted.

The welcome page refreshes connection errors and their recovery. Settings shows
the verified Host connection separately from installed versions.
**停止共享后台** suspends automatic discovery in that Desktop process until
**打开项目** succeeds (or Desktop is restarted), so its own polling does not undo
the stop.
Automatic desktop connections use `127.0.0.1:2691`; use the browser for an
explicitly configured custom-port `clat serve` instance.

- Closing the window hides it; restore through the tray/Dock or launch again.
- **退出桌面** quits only the GUI. The Host and its tasks continue.
- **停止共享后台** explicitly stops the shared Host and affects all clients.
- Uninstalling the GUI does not delete shared data, the independent CLI or an
  already running backend.

Backend executables run from versioned, digest-checked desktop cache slots.
The core owns staging and runtime leases, so updating installation files cannot
replace an executing Windows backend. Current/previous and leased runtimes are
retained. Do not manually delete a running runtime's cache.

## Desktop updates

The app checks on startup and every 24 hours; automatic checking can be disabled.
A page indicator announces available updates. Checking never downloads or installs
without a user action. The panel exposes download progress, install and cancellation.

The desktop updater and `clat upgrade` have separate installation targets and
signing roots. Neither overwrites the other's executable. Both use the same
formal release version and publication batch.

Downloaded packages require a signed release index, exact repository/version/
architecture matching and a Tauri package signature bound to the announced
version. Downgrades, malformed metadata and invalid signatures are rejected.
Installation waits for Host-owned readiness across projects. Cancellation leaves
work running; a new task racing the check keeps its original cached Host alive.

Text drafts are currently page-local. Send or clear the composer before applying
an update or opening another project. Image drafts, approvals, queues and active
runs also block backend takeover; the updater never clears them to make progress.
The text composer stays read-only while installation is waiting; cancelling the
wait restores editing.
Download/install failures leave business data intact and expose a retry path.

macOS release artifacts use Developer ID signing, Apple notarization and stapled
tickets. Windows' initial release has no Authenticode publisher signature and may
show an unknown-publisher/SmartScreen notice; desktop update signatures remain
mandatory. See [release signing](releasing.md) for the owner-side procedure.

External MCP programs still need their configured executables. Finder's PATH can
differ from a terminal's PATH; configure absolute executable paths when necessary.


## Language

The welcome page and workbench offer English, 简体中文 and 繁體中文, plus
following system languages. The desktop choice controls the embedded workbench,
native menus, notifications and update messages. It is separate from terminal
and ordinary-browser preferences. Switching keeps drafts, pending interactions
and update progress. Windows installers offer all three languages; macOS
system-owned dialogs retain the operating system's language behavior.
See [Interface languages](localization.md).

## Local desktop testing

From the repository root, run `bun start` to stage the matching private backend
and open the desktop development app. Use `bun app:build` to create a native
release-mode test installer with the current icons in `dist-desktop/`, or
`bun app:test` for packaging/release-tool contract tests. The target architecture
and workspace version are automatic. Development builds keep strict backend
identity checks; they never terminate a busy Host to make a test start.

Local test bundles have not completed production signing/notarization. Formal
publication continues to use the owner's existing signed-tag/CI/`./publish`
flow; see [release signing](releasing.md). Neither local command uploads or
publishes anything.

The desktop development and publication entry points run TypeScript directly
with Bun; they do not launch Python or a separate Node runtime. Dependencies
are pinned in `desktop/bun.lock`. The installed applications remain native
Rust programs and require none of these development tools.

The application/Dock icon derives from `desktop/icons/clat.png`; the menu-bar
(macOS) and tray (Windows) use the separate `desktop/icons/clat-stat.png`,
retaining its original colors and transparency.
