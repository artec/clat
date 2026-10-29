# CLAT desktop client

CLAT's desktop app is the native macOS/Windows client of the same local Host used by
CLAT's attached TUI and web workbench. The installer contains its own private
backend: installing a separate `clat` command is optional. The CLI remains a
single-file installation with no Tauri or user-side Node/Python requirement.

Scrollbar painting follows the actual native WebView. macOS WKWebView and Linux
WebKitGTK use custom transparent WebKit tracks, even when they accept the
standard scrollbar color property. Windows WebView2 uses standard painting when
supported, with a custom fallback; browser workbenches use capability detection.
All paths share the same activity-driven auto-hide behavior: entering or scrolling
a scrollable region reveals its thumb, which hides after about 1.2 seconds of
inactivity. Hovering or dragging a custom thumb keeps it visible. High-contrast
mode keeps scrollbars visible. Visual appearance and interaction are manually reviewed.

The composer toolbar groups add, permission, plan, context usage, model/thinking,
pending messages and send/stop controls. Toolbar icons share a centered control
size and stroke weight. Permissions are recognizable by shape and theme-aware
color: a blue eye for read-only, a green shield with a pen for project write,
and an amber unlocked padlock for full access. The permission popup opens on click or explicit keyboard activation.
Its left edge aligns with the composer's left border, with viewport avoidance.
It shows the current mode and three short descriptions, and switches permissions
directly. Enabling full access requires the existing explicit warning confirmation
inside the popup. Permission, context and pending-message popups share the project
overlay layer, so conversation clipping and the session list cannot cover them.
The add and model/thinking popups use that same layer, surface, padding, corner
radius and shadow. All composer popups sit 12px above their trigger, fit their
content and scroll within the same viewport-aware maximum height.
The model/thinking popup is centered above its trigger. Only context usage
previews on hover or keyboard focus; its unpinned preview closes shortly after
the pointer leaves, and a click pins it. Add, permission, model/thinking and
pending messages open only on click or explicit keyboard activation and stay
open when the pointer leaves. Hovering other controls never replaces an open
popup. Clicking another enabled trigger replaces it.
Click the same trigger again, click outside, move keyboard focus outside, or
press Esc to close. Only one popup is open at a time. Arrow Down enters a popup;
Tab and Shift+Tab move between its controls and the toolbar. The model popup
needs no close button. Context compaction is an explicit action inside its popup.
Pending messages use the context popup's
surface, aligned to the composer's right edge. The queue icon is disabled when
the pending count is zero; it opens the popup only when messages are present. During a
run, Send queues a message for the next model step; **Interrupt now** on a queued
item stops the current execution and automatically continues in the same
conversation. Images and fixed file references remain attached. Esc dismisses an
open popup first, otherwise stops execution and cancels any automatic handoff.
These visual and interaction details are manually accepted on each platform.

## Conversation and contextual tools

The work area has two reusable columns. **Conversation** shows the session list
and chat/composer. Project files, workspace changes, captured turn operations,
current tasks and plans/goals replace both columns with their own list and
details. The right work-area overview is temporarily hidden and restored when
returning to the conversation. File editing/saving is a separate future feature;
this file view remains a browser with fixed-snapshot quoting.

Opening a tool creates one **Conversation / current tool** tab pair. The existing
Tools menu switches that tool; tabs do not accumulate. Conversation remains
mounted with its draft, images, file references, reading position and list state.
Click Conversation to return, click the tool tab to revisit, or close the tool
for the original layout. Esc dismisses temporary UI first, then returns from a
tool to Conversation without also stopping a run. Back/forward follow view
changes. Narrow windows show a list, then details with a Back to list action.
Session changes invalidate the previous tool's ownership. Runs continue while
the user reads tools; the Conversation tab indicates activity and pending input.

The former Plan lightbulb is a click-only **Work mode** menu: Normal, Plan and
Goal. It and **Plans and goals** share one Host workflow. Plan submission first
saves a versioned Markdown document under the session's `plans/` directory.
**Approve and execute** binds that exact revision, finishes planning, and starts
same-session goal execution. **Approve only** keeps the old non-executing approval
contract; **Continue revising** retains the plan identity and creates a new draft.

The plans/goals tool has a grouped list of plan revisions, the current goal and
model-maintained to-dos. Its detail pane offers goal creation, start/continue,
pause, human acceptance and ending the goal. A human-acceptance goal stops at
**Awaiting acceptance** when the agent submits its result; it is completed only
by an explicit acceptance. **Give feedback and continue** records the feedback
and resumes the same goal. Existing machine verifiers and execution limits still
apply. Restart restores documents and progress, never silently rearms execution.
Visual appearance and all interaction are accepted manually on macOS/Windows.

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

### Windows distribution channels

The planned Microsoft Store edition is intended for general users, with
installation and updates managed by the Store. It is not available yet:
developer-account approval, app certification and the Store packaging/submission
workflow are still pending. Do not use the GitHub updater for a future Store
installation; that channel must use Store-managed updates when implemented.

The currently available GitHub edition is intended for developers and advanced
users who can verify the release source and handle their device's security
prompts and policies. It uses the built-in GitHub desktop updater or manual
installation. Its Windows installers do not currently have an Authenticode
publisher signature. SmartScreen may warn, and endpoint security or managed-device
policies may block installation entirely. CLAT update signatures verify update
integrity; they do not establish Windows publisher trust or antivirus reputation.

If security software blocks a download, keep the exact detection details and
request vendor review or an administrator's assessment of that specific file.
Do not disable system-wide protection to install CLAT. GitHub availability does
not guarantee that a device's security policy permits installation.

From v2.1.2, each macOS architecture has three files: the DMG for installation,
an App archive for automatic updates, and a combined SHA-256 list. Windows has
one installer/update EXE and its checksum. All signatures live in the shared
`latest.json`. Users of v2.1.1 or earlier must manually install v2.1.2 once;
subsequent releases use the new built-in update path.

Choose **打开项目** (Open project), select a directory and explicitly approve
project trust when requested. Model configuration, permissions, tools, sessions
and tasks use the existing workbench and the Host's normal policies.
Context usage estimation rechecks the full request during image offload only
when an image is actually removed; text-only history entries do not trigger
repeated full-history estimates when an older session exceeds its context budget.
An over-budget session still reports its estimated context usage and capacity.
If an estimate cannot be read, the usage popup shows an unavailable state instead
of remaining in the loading state.
On later launches, CLAT reopens the last project viewed in the desktop workbench
and starts its packaged backend if no Host is running. It rechecks directory
availability and trust; saved UI preferences never grant trust. If recovery
fails, the welcome page keeps the reason and lets you choose a project manually.
Desktop remembers window size and position across a full quit and relaunch.
Screen fitting leaves an already in-bounds position untouched, avoiding a
redundant native move that could overwrite pending position restoration.
The startup logo is a fixed, non-scrolling cover while restoration is pending.
Its CLAT wordmark uses gray lettering with a white spotlight sweeping from left
to right; the icon stays still. Reduced-motion and forced-color preferences use
static text, and startup proceeds as soon as restoration finishes without waiting
for an animation cycle.
The welcome page, startup cover and desktop controls use the workbench's neutral
grayscale surfaces and CLAT blue, including the `#0c416f` primary button color.
This is a single startup attempt: disconnection or **Stop shared background**
does not automatically restart the Host. Opening a project restores its normal
Host-owned session state without sending a prompt or restarting prior work.
The initial window shows a CLAT logo while discovery, project restoration and
workbench navigation are pending. It reveals project selection only when needed
or when an error requires recovery, rather than flashing that page on every launch.
Project memory is desktop-only and shares the locked UI preference file with
language and update preferences; it does not change TUI's current-directory rule.
The application toolbar uses the native title area: macOS overlays the web
content beneath the titlebar, keeps the system traffic-light controls vertically
centered with the 48px toolbar, and reserves space for them. Windows uses a custom
title area with minimize, maximize/restore, and close controls. Drag empty toolbar
space to move the window; double-click
it to maximize/restore. Interactive controls do not start dragging. Closing still
hides the window and leaves the shared Host running. On the welcome page or an
older Host without the toolbar, a compact draggable title area remains available.
Native window commands share the existing main-window, exact-origin, and verified
Host checks; no general-purpose native window API is exposed.

The toolbar and right work area remain available across content pages. Desktop
update/about actions live under Settings → Updates and about in the redesigned
workbench. macOS retains standard system editing/window menus. Completion and pending approval/question notifications contain no
conversation text; clicking one opens its project and admitted session.

Desktop controls use the page's external stylesheets on both the welcome page
and Host workbench. The bundled terminal and editor also generate runtime style
sheets for cell metrics, fonts, syntax colors and cursors. Host CSP permits these
styles while scripts remain restricted to same-origin assets, without inline
scripts or eval. Terminal font selection and a custom family name share one row
below the section heading; unavailable local font faces report an error.
The desktop panel closes with the top-right × button, which has a localized
tooltip and accessible name; closing the panel does not stop the shared Host.
Its corner positioning is owned by the native bridge and remains correct when
connected to a compatible Host whose stylesheet predates that button.
The current Windows notification plugin only reports clicks while the banner is
on screen. Historical Action Center clicks and cold-start activation are not
supported yet; restore the app through its tray when the banner has expired.

## Coexistence and background work

Desktop, CLI/TUI and PWA share the current storage root, normally `~/.clat`.
There is one Host/writer, not a second desktop database. Each project's active
session remains shared across its clients. Desktop uses one main window.
The shared UI frame has fixed navigation, one content area and an optional right
extension area. Only Conversations contains the project/session list and chat
detail; Search, Tasks, Resources, Extensions, Settings and Help own their content
pages. Returning to Conversations preserves its draft, reading position and
panel preferences.

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

On macOS, choose **CLAT → Check for updates** to open updates and check
GitHub. In the redesigned workbench, use **Settings → Updates & about** on both
platforms; native menu actions navigate to that same settings page. The welcome
page retains **Settings and about**. When connected to an older Host without the
integrated settings surface, Desktop retains its compatible standalone panel.
Windows has no application menu bar.
The panel identifies
the available desktop version and shows download progress. Choose **Download
update**, then **Install version …** after signature verification. Successful
installation restarts the desktop app. Background checks only announce updates.

The desktop updater and `clat upgrade` have separate installation targets and
signing roots. Neither overwrites the other's executable. Both use the same
formal release version and publication batch.

Downloaded packages require a signed release index, exact repository/version/
architecture matching and a Tauri package signature bound to the announced
version. Downgrades, malformed metadata and invalid signatures are rejected.
Installation waits for Host-owned readiness across projects. Cancellation leaves
work running; a new task racing the check keeps its original cached Host alive.

Text drafts are currently page-local. Before updating, send or clear unsent text
and images in every session, and finish any input-method composition. Pending
steering that might restore a draft also blocks installation. Before opening
another project through the native project picker, finish the current draft.
The in-page project list switches projects in the same window without losing
page-local drafts or stopping their runs. The native project picker still
navigates the window and requires finishing those drafts first.
The workbench bridge checks inactive projects and conversations, staged images,
unfinished submissions, composition and custom answers before its Open project,
Install and Quit actions. Installation preflight checks all owners before any
project locks its input; a rejected request leaves every project editable.
The single native update/about panel follows the active project's settings.
Checking for updates
preserves this work. Image drafts, approvals, queues and active
runs also block backend takeover; the updater never clears them to make progress.
The composer and attachment inputs stay locked while installation is waiting;
cancelling the wait restores editing. Checking and downloading can also be cancelled.
Once native installation begins, it cannot be cancelled or interrupted by quitting
the desktop app. An operation in progress excludes repeated update requests.

Download or verification failures allow another download. If native installation
returns an error before handing off to the Windows installer, the same verified
package remains available for retry during this desktop session. A restart requires
downloading it again. The panel also links to the official GitHub installer for
manual recovery. After a Windows installer takes over, its own result UI reports
any later installation failure; reopen CLAT or use the installer to retry.
Neither failure recovery nor cancellation stops the shared Host or clears its data.

macOS release artifacts use Developer ID signing, Apple notarization and stapled
tickets. The GitHub Windows edition currently has no Authenticode publisher
signature and may be warned about or blocked as described above; desktop update
signatures remain mandatory. See [release signing](releasing.md) for the
owner-side procedure.

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

Appearance is reviewed manually. CSS, icon and catalog-wording edits do not
select the Desktop native/tooling/browser jobs; language keys and parameters
still receive a lightweight check. The browser suite protects native actions,
update lifecycle and unsent work, without pixel or layout comparisons. See
[verification policy](testing.md#web-and-desktop-presentation-changes).

`bun start` uses Cargo's debug/incremental cache in the workspace target directory;
after staging the backend it uses that binary's normal `host status` / `host stop`
management commands to stop an existing Host and waits up to 30 seconds for it to
go offline before launching Desktop. This development-only restart ends running
Host tasks and terminals, just like manually invoking `clat host stop`; a failed
stop cancels startup and never force-kills the process. No discoverable Host means
there is nothing to stop. Installed Desktop and release builds keep their normal
Host lifetime policy.
It does not build a release installer. Both backend and GUI use the same native
target and build environment. Cargo still checks source freshness on every start,
but unchanged development backend resources and their manifest retain their
timestamps, avoiding needless resource rebuilds. Release packaging still stages
its resources afresh. The first build, dependency/toolchain changes, and changes
to embedded `web/` assets can require Rust recompilation. CLI-only builds without
an explicit target and test/release builds have distinct cache entries; they do
not guarantee a warm desktop development build.

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


### Navigation settings and directory selection

The workbench integrates Updates and about into its settings page. The welcome
window retains its own small native controls. Update stages show applicable actions;
exiting Desktop and stopping the shared Host remain separate operations.
`desktop_pick_directory` returns only a selected directory. It does not open a new
page, trust the directory or mount a project: the workbench performs explicit trust
and mounting after selection. Existing `desktop_open_project` keeps its whole-window
welcome/open behavior. Both commands retain the verified-origin guard.

Native open/install/quit checks include unsaved settings and task editors across all
mounted project views, in addition to conversation text, attachments, composition and
in-flight submissions. Sensitive values are not cached for navigation recovery.
Browser bridge tests are not evidence of the native OS picker or update installer.


## Reading typography

The workbench uses system sans-serif text with a shared reading scale: conversation
and historical message bodies use 14px regular text with 1.7 line height; navigation,
forms and management-page copy use 14px, secondary information 13px, and code
blocks and tool output 13px monospace. Live conversations, history and task
readers share Markdown paragraph, list, table and code styles. Markdown emphasis
keeps the regular body weight (400) to avoid dense bold patches in small text;
document headings and table headers retain medium weight (500). Typography is reviewed visually without
pixel, wording or DOM-layout CI baselines.

The compact workbench composer, extension modules, message timestamps, automatic
next steps and context/allowance popover are described in [Using CLAT](usage.md#webdesktop-conversation-reading-and-next-steps).

## Human development tools

Files and Git reuse the shared project list/detail work area; Terminal has its own
left-navigation page. They use the same Rust client ports as the browser, with no
Desktop-only save/Git/process implementation. Project editor state survives conversation
switches. Unsaved text can be quoted as an explicitly unsaved snapshot without saving
the file. Terminal appearance is configurable in Settings, with a live glyph preview and local system font selection. No font files are
bundled. See [usage](usage.md#project-editor-git-and-terminals)
for workflows and recovery boundaries. Native macOS/Windows font, IME, keyboard and
interactive terminal behavior is manually accepted separately from compilation.

The welcome page uses the application's system UI typeface and responsive heading
sizes. Headings wrap to the available width rather than using translation-embedded
line breaks; status details remain readable and the Open Project button keeps its
label and arrow together with consistent spacing.

Desktop suppresses the WebView's default context menu across the welcome page and
workbench. Application context menus continue to receive right-click events.
Visual focus rings are hidden; DOM focus, text carets and keyboard navigation remain.

### Tray navigation

On macOS and Windows, left-clicking the CLAT tray icon shows the main window;
right-clicking opens a native menu with Recent, Settings and Quit CLAT. Recent
contains the last three conversations opened in this desktop client across all
projects, labeled with conversation title and project name. Opening a conversation
moves it to the top; a title update does not change the order. The list is retained
in private desktop preferences, independently of session modification times and
Agent execution. Newly enabled history accumulates as conversations are opened.
Recent is a disabled group heading; the conversation entries below it are clickable.
The native bridge records opened conversations through explicitly permitted Tauri
commands; bridge failures are reported in desktop settings instead of being ignored.

Selecting a recent conversation uses the existing project/session navigation and
trust checks. Settings opens the existing settings view. Quit follows normal
desktop exit/update safeguards and does not implicitly stop a shared Host. Native
menu labels follow the desktop language preference.

Terminal settings query the viewing device font library through the guarded
`desktop_fonts` command, backed by Core Text on macOS and DirectWrite on Windows.
Only family, face names, PostScript identifiers and monospace metadata cross the
bridge; no font files or filesystem paths are exposed. The selector uses local
font faces and validates loading before updating terminal appearance.
