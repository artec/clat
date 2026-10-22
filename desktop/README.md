# CLAT Desktop development

This optional Rust workspace member is a native shell for the existing Host.
Business clients use `clat-core::client_ports`; no Application is constructed here.
See [desktop operation](../docs/desktop.md) and [release signing](../docs/releasing.md).

The root Cargo.toml workspace version is authoritative. Tauri config deliberately
omits a separately maintained version; the build wrapper supplies the workspace
version. Bun runs the TypeScript development/release tooling; installed apps
need no script runtime. Bun 1.3.14 and `desktop/bun.lock` pin the tool runtime
and dependencies. There is no Python or separate Node invocation in this path.

Run from the repository root (or from `desktop/`):

```sh
bun start       # Stage the native private backend, then launch Tauri development mode
bun app:build   # Build/package this machine's release-mode test installer
bun app:test    # Run desktop packaging/release-tool contract tests
```

`start`/`app:build` detect macOS/Windows and x64/ARM64 automatically, install the
pinned Tauri development CLI if missing, and derive the version from Cargo.toml.
The default CLI/core Cargo build is unchanged. Bun and the Rust/native build
toolchain are local development tools; installed desktop/CLI users do not need them. `app:build`
writes to the root `dist-desktop/` and includes current application icons. It
does not run production signing, notarization, upload or publication. Development
mode uses embedded pages rather than widening native bridge origins for a dev
server. Its strict development-backend identity checks remain in force.

The owner release flow is still **push the reviewed signed tag → wait for CI →
`./publish`**. CI builds six CLI targets and four desktop targets natively. The
single owner command collects them, signs/notarizes locally, verifies all assets,
uploads to the draft, then publishes. Desktop key password input is hidden and
automatic; CLI Minisign unlocks once. If upload fails, rerun `./publish`: verified
local signed files under `dist-release/<tag>/prepared/` are reused. Do not run the
owner command as a development smoke test.

Advanced verification commands:

```sh
bun install --cwd desktop --frozen-lockfile --ignore-scripts
bun desktop/scripts/release.ts build --target aarch64-apple-darwin
cargo test -p clat-desktop --locked
cargo clippy -p clat-desktop --all-targets --locked -- -D warnings
bun app:test
bun desktop/node_modules/typescript/bin/tsc --project desktop/tsconfig.json
npm ci --prefix web/e2e --ignore-scripts
node web/e2e/node_modules/@playwright/test/cli.js install chromium
npm --prefix web/e2e test -- --config=playwright.desktop.config.js
```

For the shipping build add `--release --official`. The wrapper builds/stages the
private CLI backend first, writes its version/target/digest manifest, then builds
the shell. It respects Cargo's target directory and strips inherited Apple/Tauri
signing variables. Only the owner's finalizer signs production artifacts.

Debug builds support `CLAT_DESKTOP_SMOKE=1` for isolated window verification:
backend preparation/discovery, notification subscriptions and update timers are
skipped. This cannot connect to or start the real Host. The mode is disabled in
release builds. Ordinary development builds use strict backend fingerprints;
do not use a real busy Host as a development fixture.

Release matrix and asset names are owned by `assets.json`. `scripts/release.ts`
provides matrix/names/build/package/finalize operations. CI stages unsigned
artifacts; the repository owner's `publish` performs final Apple signing,
notarization, independent updater signing, checksum signing and publication.

The desktop public update key is committed in `updater.pub`; its separate private
key lives only at `.release-secrets/clat-desktop.key`. Back it up securely. Never
export another application's TAURI_SIGNING_PRIVATE_KEY for CLAT. Run
`bun desktop/scripts/protect-key.ts` in a local terminal to add/change its
password without changing the public key. Input is hidden, an empty new password
is refused, and failure leaves the original key intact. For the initial empty
password, press Enter at the old-password prompt. Release signing accepts
CLAT_DESKTOP_KEY_PASSWORD; enter it with a hidden terminal prompt rather than
putting it in shell history. Apple credentials are local-only;
CLAT_NOTARY_PROFILE takes precedence over the existing Apple account variables.
No development command should run `publish` or `finalize` as a smoke test.

Notification transport is host-wide so it does not pin idle project domains.
The stream carries metadata, not conversation text. Notification click routing
uses the admitted session ID frozen by core, not a GUI guess of current selection.


Application icons are generated from the owner-provided `icons/clat.png`.
Preserve its existing transparent margins and rounded artwork; do not substitute
the website favicon or add another mask. The pinned Tauri CLI's `icon` command
generates `icon.icns` (macOS), `icon.ico` (Windows), and `icon.png` (native window image). Only these desktop outputs are retained; mobile assets are
not used. The independent tray/status-bar image is `icons/clat-stat.png`,
embedded at compile time with its supplied colors and transparency; it is not
a macOS template mask. Rebuild the application bundles after changing icons.

Archive handling uses a pinned development-only `tar` package to preserve native
executable modes and PAX metadata while rejecting traversal, links and special
files before extraction. Release locking uses the owner machine’s native flock
through Bun FFI; no Python helper is launched.
