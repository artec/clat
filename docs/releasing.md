# Release signing

This document is for the repository owner publishing CLAT releases. It defines
the artifact trust chain, offline signing procedure, rollback behavior, key
rotation, and platform baseline. Plugin-market releases use the separate
[market publishing/signing runbook](../market/PUBLISHING.md) and
[FTP deployment runbook](../market/FTP-DEPLOY.md).

## Compact release assets (v2.1.2 onward)

CLAT's shared Host remains the core product; Desktop is an optional frontend.
A formal release publishes exactly **23 assets**:

| Product | Per platform | Total |
| --- | --- | --- |
| CLI, six targets | archive + archive.sha256 | 12 |
| macOS Desktop, two architectures | DMG + app.tar.gz + one combined .sha256 | 6 |
| Windows Desktop, two architectures | setup.exe + setup.exe.sha256 | 4 |
| Shared release index | latest.json | 1 |

The macOS checksum file is named `clat-desktop-vX.Y.Z-macos-<arch>.sha256`
and contains both the DMG and updater archive hashes. The Windows installer is
also its updater package; no duplicate download is needed. There are no public
`.sig`, `.minisig`, `desktop-latest.json` or per-signature checksum files.
Intermediate signatures remain local and are excluded by the upload allowlist.

### Trust model

`latest.json` is an envelope with `schema_version: 1`, a `payload` string
and `signatures.cli` / `signatures.desktop`. The payload is the **exact UTF-8
JSON text that was signed**, avoiding JSON reserialization or a self-signature
cycle. It contains version, notes, publication time, four Tauri platform entries
(including package signatures), and an `artifacts` map of all 22 downloadable
packages/checksum files to SHA-256 digests.

The CLI verifies the payload using its unchanged embedded release public key;
its signature-covered comment must equal `CLAT <tag> release index`.
The CLI then matches the signed version, exact asset name, archive hash and
checksum-file hash, and validates the one-entry checksum file before extracting.
All three download URLs must match this repository and the exact release tag.
Desktop verifies the same payload with its independent embedded desktop public
key, then passes the authenticated platform metadata to the official updater.
The updater still requires a version-bound package signature and rejects
substitution or downgrade. No trust keys are rotated by this format change.

### Transition and first installation

**Clients before v2.1.2 must be reinstalled manually once.** The owner will notify
the existing users; neither the old CLI `upgrade` path nor the old desktop updater
can consume this new format. Do not retain obsolete sidecars for compatibility.
Clients from v2.1.2 onward use the compact format exclusively.

The normal first-install scripts continue to use HTTPS and SHA-256; they do not
provide an independent signing-key bootstrap. For manual verification, obtain
`release/minisign.pub` from a trusted independent channel and authenticate the
index before using its hashes. Example (macOS; use sha256sum on Linux):

```bash
CLAT_TAG=v2.1.2
CLAT_ASSET="clat-${CLAT_TAG}-aarch64-apple-darwin.tar.gz"
CLAT_URL="https://github.com/artec/clat/releases/download/${CLAT_TAG}"
curl -fLO "${CLAT_URL}/${CLAT_ASSET}"
curl -fLO "${CLAT_URL}/latest.json"
# -j preserves the exact signed text; do not pretty-print the payload.
jq -j '.payload' latest.json > release-payload.json
jq -j '.signatures.cli' latest.json > release-payload.json.minisig
minisign -Vm release-payload.json -x release-payload.json.minisig -p /trusted/clat-minisign.pub
test "$(sed -n '3p' release-payload.json.minisig)" = "trusted comment: CLAT ${CLAT_TAG} release index"
test "$(jq -r '.version' release-payload.json)" = "${CLAT_TAG#v}"
CLAT_EXPECTED=$(jq -er --arg n "$CLAT_ASSET" '.artifacts[$n]' release-payload.json)
test "$(shasum -a 256 "$CLAT_ASSET" | cut -d ' ' -f1)" = "$CLAT_EXPECTED"
```

Windows PowerShell equivalent after downloading the archive and latest.json:

```powershell
$ClatTag = 'v2.1.2'
$ClatAsset = "clat-$ClatTag-x86_64-pc-windows-msvc.zip"
$Envelope = Get-Content -Raw -Encoding UTF8 latest.json | ConvertFrom-Json
$Utf8 = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText("$PWD/release-payload.json", $Envelope.payload, $Utf8)
[IO.File]::WriteAllText("$PWD/release-payload.json.minisig", $Envelope.signatures.cli, $Utf8)
& minisign -Vm release-payload.json -x release-payload.json.minisig -p C:\trusted\clat-minisign.pub
if ($LASTEXITCODE -ne 0) { throw 'Invalid release signature' }
$Comment = [IO.File]::ReadAllLines("$PWD/release-payload.json.minisig")[2]
if ($Comment -cne "trusted comment: CLAT $ClatTag release index") { throw 'Wrong release tag' }
$Payload = $Envelope.payload | ConvertFrom-Json
if ($Payload.version -cne $ClatTag.Substring(1)) { throw 'Wrong release version' }
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $ClatAsset).Hash.ToLowerInvariant() -cne $Payload.artifacts.$ClatAsset) { throw 'Archive checksum mismatch' }
```

## Maintainer prerequisites

Publishing requires:

- a POSIX shell with Bash, Bun 1.3.14 and the normal repository build toolchain;
- `gh`, authenticated as an account allowed to edit releases in `artec/clat`;
- `minisign`, supporting `-S`, `-R`, and `-V`;
- the encrypted private key at `.release-secrets/clat-minisign.key`;
- a reviewed version tag whose CI workflow has created an unsigned draft.

The repository does not pin minimum `gh` or Minisign versions. Use maintained
releases and verify the installed commands before starting:

```bash
gh --version
env -u GH_TOKEN -u GITHUB_TOKEN gh auth status
minisign -v
test -f .release-secrets/clat-minisign.key
chmod 600 .release-secrets/clat-minisign.key
```

Run `./publish` from the repository root on the maintainer-controlled machine.
The command selects the exact workspace-version draft, checks the complete
artifact set, and performs browser login when the GitHub CLI credential store is
empty. The desktop addition does not require a separate publication command.
`./publish` delegates directly to Bun/TypeScript; desktop build, packaging,
signing and key-password tools have no Python dependency. Dependencies are
installed from the single `desktop/bun.lock` with frozen resolution.

## Private key handling

The private key lives at `.release-secrets/clat-minisign.key`, a Git-ignored
local path. It must never enter GitHub Actions, repository secrets, build logs,
or release assets.

Before every release:

1. Confirm an encrypted offline backup exists.
2. Derive the public key from the private key:

   ```bash
   minisign -R \
     -s .release-secrets/clat-minisign.key \
     -p /tmp/clat-release.pub
   ```

3. Compare the base64 public-key line with `release/minisign.pub`.

An environment approval gate inside GitHub Actions is not an offline signing
boundary. Do not upload the key there.

## Publish procedure

### 1. Prepare the tree

- Bump the version once in the root `Cargo.toml` `[workspace.package]`
  section — both packages inherit it (`version.workspace = true`), so there
  is no second copy to keep equal; both embed the release version in
  protocol/client strings.
- Run the full Rust and package test gates appropriate to the release.
- Perform [live-model validation](live-validation.md) when provider/runtime
  behavior changed.
- Review the exact commit that the version tag will identify.

The repository owner performs all commits, tag pushes, and release publication.

### 2. Create the draft

Push the version tag. [The release workflow](../.github/workflows/release.yml)
builds the platform matrix and creates an unsigned **draft** release. Drafts do
not appear in the latest-release endpoint used by installers or `clat upgrade`.

Confirm:

- the workflow ran from the intended tag commit;
- every expected archive exists;
- every one-entry SHA-256 manifest exists;
- build/test results match the reviewed source.

A signature authenticates the maintainer's decision; it cannot turn an
unreviewed build into a trustworthy build.

### 3. Sign and publish offline

Run from the maintainer machine:

```bash
./publish
```

The script:

1. selects the draft matching the root Cargo.toml version;
2. derives the complete expected CLI/desktop set from `desktop/assets.json`;
3. refuses to continue while an archive or manifest is missing;
4. downloads all archives/manifests and checks exact names and bytes;
5. signs/notarizes desktop assets locally, creates the signed updater index,
   then signs the shared payload with the CLI key and embeds both index signatures;
6. verifies both trust roots, version bindings and all four index destinations;
7. uploads only the expected files, downloads them again to compare with the
   verified local copies, and publishes only after all checks succeed.

An interrupted upload/publication can be retried with the same `./publish`.
Verified local files under `dist-release/<tag>/prepared/` are reused, avoiding
repeat signing/notarization. To intentionally rebuild a prepared version, move
that version's `prepared/` directory aside first. These local directories are
Git-ignored and contain release assets, not private keys. A published release is
never silently edited as though it were still a draft.

If GitHub CLI has no stored credential, the script opens device login and
copies the one-time code to the clipboard. Inside its child processes it
ignores `GH_TOKEN` and `GITHUB_TOKEN` so an ambient CI-style token cannot
silently choose another identity; it does not delete or alter the caller's
environment variables.

## Upgrade replacement and rollback

After verification, `clat upgrade` stages the new executable inside the install
directory before replacing the current binary.

On platforms with a direct atomic replacement path, failure leaves the current
binary intact. Windows uses a two-step swap; if the second step fails, CLAT
forces rollback to the previous executable. A failed upgrade must never leave
the installation without a runnable binary.

## Key rotation

Do not rotate by replacing `release/minisign.pub` and immediately signing only
with the new key. Existing installed binaries would reject the release needed
to learn that key.

Use a transition sequence:

1. release a binary signed by the old key that trusts both old and new public
   keys;
2. switch release signing to the new private key;
3. after the installed population has a migration path, remove the old public
   key in a later release.

Treat loss or suspected compromise of the private key as a release incident.
Pause publication until the transition and user communication plan is explicit.

### Plugin Index signing

The v1 `pi.at.cn` machine index deliberately reuses this offline CLAT release
trust anchor; it never uses a publisher key or a CI secret. This avoids shipping
an unmanageable second production private key during the first market release,
but it also joins the incident boundary: suspected compromise pauses both
binary releases and market-index publication, followed by fresh revocation and
vulnerability records once trust is restored.

Run `market/scripts/release-index.mjs` only on the offline release-signing
machine and point it at the same Git-ignored key. The script validates local
artifact hashes, writes a seven-day index and binds the market id/generation in
the Minisign trusted comment. Deploy `market/dist/` only after local signature
verification. A future dedicated market root must use the same overlapping-key
binary-release sequence above; replacing an embedded public key in-place would
strand installed clients.

## Platform baseline

The workflow currently builds:

| Asset target | Runner | Supported baseline |
|---|---|---|
| `aarch64-apple-darwin` | `macos-latest` | oldest macOS targeted by the current Rust toolchain |
| `x86_64-apple-darwin` | `macos-latest` | oldest macOS targeted by the current Rust toolchain |
| `x86_64-pc-windows-msvc` | `windows-latest` | Windows 10+ |
| `aarch64-pc-windows-msvc` | `windows-latest` | Windows 10+ |
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | glibc 2.39+ |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | glibc 2.39+ |

Linux runners are pinned to the lowest supported generation instead of
`ubuntu-latest`. Raising the glibc baseline requires an explicit documented
decision, normally only when GitHub retires the pinned runner.

Users on older Linux distributions can build the shipped CLAT core binary from
source with the stable Rust toolchain. The core uses rustls and has no SQLite
system dependency. Optional user-configured MCP servers and the separate DSH
adapter package may require their own runtimes.

## Release verification checklist

Before announcing a release, confirm:

- the GitHub release is no longer a draft;
- every target has archive, checksum, and signature;
- a current installed CLAT reports the expected `clat --version` after
  `clat upgrade`;
- a tampered archive, manifest, tag binding, or signature is rejected in test;
- first-install scripts still verify the SHA-256 manifest;
- README platform claims match the workflow matrix.

## Desktop-enabled formal releases

From the first desktop release, every formal release includes six CLI targets and
four desktop targets. `desktop/assets.json` is the typed inventory used by CI and
`publish`; target strings must never be scraped indiscriminately from workflow
YAML. Desktop filenames omit full CLI Rust target triples so older CLI selectors
cannot mistake installers for CLI executables. There is no preview update channel.

The root workspace Cargo.toml remains the only manually maintained version.
CI marks release builds with CLAT_OFFICIAL_RELEASE=1, builds six CLI targets and
two unsigned Windows desktop installers, and waits for those jobs before creating
the draft Release. The owner's Mac builds both macOS desktop architectures locally
with the same official build identity. Versioned cache runtimes keep running independently of the
installation directory. Build tools do not become CLI runtime dependencies.

Before owner-side publication, install desktop build tools with
`bun install --cwd desktop --frozen-lockfile --ignore-scripts`. The separate desktop private key is
`.release-secrets/clat-desktop.key`; the compiled public key is `desktop/updater.pub`.
The initial key has been generated locally with owner-only file permissions.
Back up the private key securely before distribution. It is never uploaded to CI.
To protect the existing key without rotating the public key, run
`bun desktop/scripts/protect-key.ts` in your local interactive terminal.
The old password is initially empty (press Enter). The script hides password
input, refuses an empty new password, verifies the unchanged public identity,
and atomically saves the encrypted Tauri-format key with permissions 0600 only
after verification. Failure leaves the original key intact. Back up the protected
key and keep its password separately; losing either prevents future signing.

`./publish` asks for the desktop key password using hidden terminal input;
Minisign then asks for the CLI key password once for the shared release index.
Passwords never need to be typed into a command or placed in shell history.
An existing CLAT_DESKTOP_KEY_PASSWORD environment value remains supported.

The desktop signer passes the password through its child environment rather than
process arguments. Do not reuse another application's Tauri signing variables or
the CLI Minisign key.
It normalizes the key's outer Base64 wrapper (including trailing LF or wrapped
CRLF) in a temporary file with permissions 0600 before invoking Tauri's strict
signer. The encrypted key bytes and original private-key file stay unchanged;
the temporary copy is removed after success or failure. No key rotation or
manual editing of the backed-up key is required for this formatting issue.

The owner still runs just `./publish` on macOS after pushing the signed tag;
there is no need to wait for the complete CI draft first. No separate Bun
release/upload/publish commands are required.
Local formal builds require a clean worktree with HEAD equal to the current
version tag. The script selects the exact tag/commit's Release run and waits only
for each required macOS CLI artifact, then compiles its desktop App locally while
other remote platforms continue. The private backend reuses those CLI bytes;
it is not compiled again locally. Pinned macOS targets are installed as needed.
For each macOS architecture, the terminal reports elapsed time for preparation
and compilation/packaging, Apple signing/notarization/installer creation, and the
total, followed by a two-architecture summary. Preparation includes waiting for
and downloading the verified CLI backend; these are wall-clock durations, not
CPU compilation benchmarks. Failed stages report elapsed time before preserving
the original error. Reused stages are explicitly labelled; totals measure only
the current invocation, not the original cached build. No signing credentials
or release assets are changed by timing.
Once both local macOS stages and the remote Release finish, the script validates
the complete draft and downloads the six CLI archives and two Windows installers.
Signing and finalization follow this order:

1. Sign each nested backend using the local Developer ID Application identity and
   backend-only WASM/JIT entitlements; refresh its post-signing digest manifest.
2. Sign the App with hardened runtime, notarize it and staple/validate its ticket.
3. Build the final DMG with the App and Applications shortcut, sign/notarize it,
   then staple/validate it. Make updater archives from the stapled App.
4. Sign Windows installers and macOS updater archives with the independent desktop
   key and an explicit signed app version; verify signatures locally.
5. Write combined desktop checksums and the exact release payload, sign it with
   both release roots, embed all signatures in `latest.json`, then upload only
   the 23 public assets and publish after exact remote inventory/byte verification.

The available local identity is `Developer ID Application: Artec Inc. (VJUBRKV7K9)`.
APPLE_SIGNING_IDENTITY can select another authorized identity. CLAT_NOTARY_PROFILE
uses a Keychain notarytool profile; otherwise the finalizer accepts the existing
APPLE_ID, APPLE_TEAM_ID and APPLE_APP_SPECIFIC_PASSWORD/APPLE_PASSWORD environment
configuration. Arguments containing secrets are not logged. Build subprocesses
strip inherited Apple/Tauri signing variables; only the explicit local finalizer
performs production signing. Never finalize partially built releases.
The finalizer preflights tools, the local certificate, notarization configuration
and the desktop key pair before modifying artifacts. DMGs use a simple
App/Applications layout produced by hdiutil, without Finder automation.

The unsigned macOS build produces only an App archive, without a throwaway DMG.
The final DMG is made once, after signing and notarizing the App. Cargo's local
target directory is retained across releases. Completed unsigned and Apple-signed
stages are stored under `dist-release/vX.Y.Z/macos/<commit>/<target>/`; each receipt
binds the source commit, target, stage and exact file hashes. Interrupted
notarization reuses a completed compilation; interruption after signing reuses the
signed stage. Changes to cached bytes or source identity reject reuse. A failure
inside a stage retries that stage; this does not promise resumption within an Apple
notarization submission. Do not delete local build caches between retries.

Each Windows CLI and desktop installer share one platform job: compile CLI once,
save a `backend-<target>` artifact immediately, package CLI, and build the desktop
using that same executable. Desktop does not invoke another `cargo build -p clat`.
The artifact contains an official-build receipt binding commit, version, target,
workflow run, size and SHA-256. The producer checks the executable's version;
consumers check the receipt and PE/Mach-O architecture before staging bytes.
Artifact downloads may lose executable mode, which staging explicitly restores.
If the same Release run is retried after desktop failure, its validated saved CLI
is restored instead of rebuilt. Missing artifacts trigger the normal CLI build;
invalid artifacts fail closed, without a silent rebuild fallback. Internal
`backend-*` artifacts are excluded from public Release assets.

The owner command polls for tag dispatch, CLI availability and final completion
with bounded waits (up to 90 minutes per wait). Remote failures or timeouts leave
completed local stages intact; retry failed jobs and rerun `./publish`. It does
not start, cancel or rerun GitHub jobs automatically. A manual Release run must
select the version tag, rather than a branch. This parallelizes local desktop
work with remaining remote builds; it does not remove compilation of the GUI or
promise a particular cold-build duration.

### Release dependency cache

The six-platform `plugin-performance` CI matrix uses the same native runner
labels as the release inventory. Its four macOS/Windows targets and Release use
the same rust-cache prefix `v4-product-native` and shared key `product-<target>`.
Compiler/environment and
Cargo manifest/lockfile hashes remain enabled; operating systems, architectures,
toolchains and incompatible flags are not mixed to force a hit. This replaces
the unrelated job-based cache keys that previously isolated CI from Release.

Linux targets skip this shared cache and its extra warming in both CI and Release;
their CLI builds and existing checks remain enabled. Ordinary Linux test-job
caches are separate and retain their existing policy. This reserves the shared
cache capacity for the four desktop targets.

Only successful default-branch CI runs with native input changes write this
shared cache. Those jobs run desktop clippy/tests and tooling checks on all four
native platforms before saving; script-only runs cannot publish an incomplete
immutable snapshot. On an exact cache miss in those native-input runs, including
configuration changes restored through a fallback, those jobs build the
complete CLI with exactly Release's `cargo build --release --locked --target ...`
command and, on Windows, the desktop executable with `tauri/custom-protocol`
in a separate invocation. Core-only warming is insufficient: CLI/TUI dependencies
enable additional shared dependency features (for example ratatui/lru/hashbrown),
which change Wasmtime/Cranelift dependency fingerprints. The separate GUI build
keeps its own feature variant; combining both products in one Cargo invocation
would warm a third union instead of their individual release graphs.
These warm builds create no installers or release assets and
use no signing credentials. Exact hits skip these extra builds. Existing SHA
checks now use the same explicit target directory as Release, avoiding a second
host-default release directory. The cache action retains dependency artifacts,
not workspace binaries; real release code is still compiled and validated.

The v4 generation replaces v3's incomplete core-only snapshots. A v3 cache hit
must not skip the corrected warm build. The first successful native-input main
CI therefore prepares v4 before a tag is pushed; this change does not delete
remote caches or trigger jobs automatically. Cache restoration and Cargo artifact
freshness are separate observations: a downloaded cache alone does not establish
that expensive dependencies were reused.

Tag releases restore the default branch's cache without saving another four
large copies under each tag. Tags cannot read caches from different tags.
Initially, let the default-branch CI finish warming before creating the next
tag. After dependency/toolchain changes, warming may be needed again. If a tag
starts first, or GitHub evicts the cache, the build still performs an ordinary
cold compilation; release validation and artifact identity checks are unchanged.
Source-only pushes with an exact key do not repeatedly warm optimized binaries.
This reduces repeated dependency work rather than the cost of all linking/LTO;
actual cache-hit rates and end-to-end savings require remote measurements.

Automatic desktop clippy/tests now run inside CI's existing four macOS/Windows
platform jobs, using the same explicit target directory and cache as the core
checks and Release preparation. Desktop no longer starts four duplicate native
jobs for a push/PR. Its workflow provides web-shell and fast tooling feedback;
the native entry is manual-only. Both workflows plan from the complete push diff
or PR base-to-tested-merge diff. Rust/core, native resources/configuration,
toolchain/workflow and unknown inputs retain all four native checks; webpage-only
changes select web tests, and script-only changes select tooling (including all
four native-platform Bun suites in CI). Missing history selects all checks.
Ordinary Linux and Windows product gates and the six-platform compile/performance
matrix are unchanged. Native checks do not generate installers.

CI native-platform jobs, manual Desktop checks and Release cache readers agree on
`CARGO_PROFILE_DEV_DEBUG=0` and `CARGO_PROFILE_TEST_DEBUG=0`. This reduces debug
metadata and cache/linking overhead; optimization, assertions, overflow checks,
test selection and local developer profiles are unchanged. A fresh cache generation
is necessary once after this change; actual savings must be measured remotely.
Manual checks and tag builds only read the default-branch cache. New pushes cancel
superseded CI/Desktop runs. For an isolated Windows test installer, use the
Desktop workflow's manual Run workflow entry, select a target
and enable `installer`; its artifacts are not formal release inputs. For macOS
use `bun app:build` locally. Tagged Release builds still require the full formal
asset inventory before `./publish` makes the draft public. Re-run failed jobs to
preserve successful platform jobs when retrying the same commit.

Windows initially uses per-user NSIS installers without Authenticode. All desktop
updates still require signatures. The updater validates a signed, version-fixed
index, exact repository/platform URLs and signed package versions, with downgrades
disabled. CLI `clat upgrade` only selects CLI assets and retains its existing trust
root. Desktop and CLI installation/update actions are independent; release versions
and batches are shared. Private-key loss requires a manual recovery install; key
rotation requires a transition release signed by the old trusted key.
