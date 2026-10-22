# Release signing

This document is for the repository owner publishing CLAT releases. It defines
the artifact trust chain, offline signing procedure, rollback behavior, key
rotation, and platform baseline. Plugin-market releases use the separate
[market publishing/signing runbook](../market/PUBLISHING.md) and
[FTP deployment runbook](../market/FTP-DEPLOY.md).

## Trust model

Each release archive has two companion files:

```text
<asset>
<asset>.sha256
<asset>.sha256.minisig
```

The SHA-256 manifest contains exactly one expected asset entry. The Minisign
signature authenticates that manifest with the public key embedded in the
installed CLAT binary from [`release/minisign.pub`](../release/minisign.pub).

`clat upgrade` fails closed unless all of these match:

- release tag;
- expected asset name;
- one-entry manifest shape;
- archive SHA-256;
- Minisign signature;
- signature-covered trusted comment
  `CLAT <tag> release checksum manifests`.

Binding the tag in the trusted comment prevents a genuinely signed historical
asset from being replayed under a newer tag.

## First install versus upgrade

An installed CLAT binary has an immutable embedded trust root, so subsequent
`clat upgrade` checks survive compromise of GitHub assets or the CDN unless the
attacker also obtains the offline private key.

The default curl/PowerShell first-install path has a weaker bootstrap boundary:
it fetches the installer and SHA-256 manifest from GitHub over HTTPS, but does
not independently authenticate that manifest with Minisign. An attacker able
to replace the repository/script can replace both archive and checksum.

For account-compromise-resistant first installation, obtain the public key
through an independent channel and verify the downloaded manifest manually, or
use a package manager with its own signed trust chain.

### Manual first-install verification

After obtaining `release/minisign.pub` through an independent trusted channel,
download one archive, its manifest, and signature from the same release. For
example on macOS arm64:

```bash
CLAT_TAG=v0.9.2
CLAT_TARGET=aarch64-apple-darwin
CLAT_ASSET="clat-${CLAT_TAG}-${CLAT_TARGET}.tar.gz"
CLAT_RELEASE_URL="https://github.com/artec/clat/releases/download/${CLAT_TAG}"

curl -fLO "${CLAT_RELEASE_URL}/${CLAT_ASSET}"
curl -fLO "${CLAT_RELEASE_URL}/${CLAT_ASSET}.sha256"
curl -fLO "${CLAT_RELEASE_URL}/${CLAT_ASSET}.sha256.minisig"

minisign -Vm "${CLAT_ASSET}.sha256" \
  -x "${CLAT_ASSET}.sha256.minisig" \
  -p /path/from-independent-channel/clat-minisign.pub

# A Minisign signature is exactly four lines. The successful verification
# above authenticates line 3 through the global signature; the structural
# checks below also reject appended or substituted look-alike comments.
CLAT_SIGNATURE="${CLAT_ASSET}.sha256.minisig"
test "$(awk 'END { print NR }' "${CLAT_SIGNATURE}")" -eq 4
case "$(sed -n '1p' "${CLAT_SIGNATURE}")" in
  "untrusted comment:"*) ;;
  *) exit 1 ;;
esac
test -n "$(sed -n '2p' "${CLAT_SIGNATURE}")"
test "$(sed -n '3p' "${CLAT_SIGNATURE}")" = \
  "trusted comment: CLAT ${CLAT_TAG} release checksum manifests"
test -n "$(sed -n '4p' "${CLAT_SIGNATURE}")"

# The signed manifest must have one non-empty line and name this asset.
test "$(awk 'NF { n++ } END { print n + 0 }' "${CLAT_ASSET}.sha256")" -eq 1
read -r CLAT_EXPECTED CLAT_PUBLISHED CLAT_EXTRA < "${CLAT_ASSET}.sha256"
CLAT_PUBLISHED=${CLAT_PUBLISHED#\*}
test "${#CLAT_EXPECTED}" -eq 64
case "${CLAT_EXPECTED}" in *[!0-9A-Fa-f]*) exit 1 ;; esac
test "${CLAT_PUBLISHED}" = "${CLAT_ASSET}"
test -z "${CLAT_EXTRA:-}"

# Verify the archive bytes against the now-authenticated manifest.
shasum -a 256 -c "${CLAT_ASSET}.sha256"   # macOS
# sha256sum -c "${CLAT_ASSET}.sha256"     # Linux
```

Substitute the release tag and target. The equivalent Windows PowerShell flow
is executable as follows:

```powershell
$ClatTag = 'v0.9.2'
$ClatTarget = 'x86_64-pc-windows-msvc'
$ClatAsset = "clat-$ClatTag-$ClatTarget.zip"
$ClatReleaseUrl = "https://github.com/artec/clat/releases/download/$ClatTag"
$ClatPublicKey = 'C:\trusted\clat-minisign.pub'

Invoke-WebRequest "$ClatReleaseUrl/$ClatAsset" -OutFile $ClatAsset
Invoke-WebRequest "$ClatReleaseUrl/$ClatAsset.sha256" `
  -OutFile "$ClatAsset.sha256"
Invoke-WebRequest "$ClatReleaseUrl/$ClatAsset.sha256.minisig" `
  -OutFile "$ClatAsset.sha256.minisig"

& minisign -Vm "$ClatAsset.sha256" `
  -x "$ClatAsset.sha256.minisig" `
  -p $ClatPublicKey
if ($LASTEXITCODE -ne 0) { throw 'Minisign verification failed' }

# Successful Minisign verification authenticates line 3. Requiring exactly
# four lines prevents an appended, unauthenticated look-alike comment.
$SigLines = [IO.File]::ReadAllLines(
  (Resolve-Path "$ClatAsset.sha256.minisig")
)
if ($SigLines.Count -ne 4) { throw 'Invalid signature structure' }
if (-not $SigLines[0].StartsWith('untrusted comment:')) {
  throw 'Invalid signature header'
}
if ([string]::IsNullOrWhiteSpace($SigLines[1]) -or
    [string]::IsNullOrWhiteSpace($SigLines[3])) {
  throw 'Invalid signature payload'
}
$ExpectedComment = "trusted comment: CLAT $ClatTag release checksum manifests"
if ($SigLines[2] -cne $ExpectedComment) {
  throw 'Release tag is not bound by the trusted comment'
}

$ManifestLines = [IO.File]::ReadAllLines(
  (Resolve-Path "$ClatAsset.sha256")
)
$Entries = @($ManifestLines | Where-Object { $_.Trim().Length -gt 0 })
if ($Entries.Count -ne 1) { throw 'Invalid manifest entry count' }
$ManifestMatch = [regex]::Match(
  $Entries[0],
  '^([0-9A-Fa-f]{64}) [ *](.+)$',
  [Text.RegularExpressions.RegexOptions]::CultureInvariant
)
if (-not $ManifestMatch.Success) { throw 'Invalid manifest structure' }
$ExpectedHash = $ManifestMatch.Groups[1].Value.ToLowerInvariant()
if ($ManifestMatch.Groups[2].Value -cne $ClatAsset) {
  throw 'Unexpected manifest asset name'
}
$ActualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $ClatAsset).Hash
if ($ActualHash.ToLowerInvariant() -cne $ExpectedHash) {
  throw 'Archive checksum mismatch'
}
```

Use `aarch64-pc-windows-msvc` instead on Windows arm64. Do not fetch the
"independent" public key from the same potentially compromised repository
session as the artifacts.

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
   then signs all checksum manifests with the CLI key;
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
Minisign then asks for the CLI key password once for all checksum manifests.
Passwords never need to be typed into a command or placed in shell history.
An existing CLAT_DESKTOP_KEY_PASSWORD environment value remains supported.

The desktop signer passes the password through its child environment rather than
process arguments. Do not reuse another application's Tauri signing variables or
the CLI Minisign key.

The owner still runs just `./publish` on macOS after pushing the signed tag;
there is no need to wait for the complete CI draft first. No separate Bun
release/upload/publish commands are required.
Local formal builds require a clean worktree with HEAD equal to the current
version tag. The script selects the exact tag/commit's Release run and waits only
for each required macOS CLI artifact, then compiles its desktop App locally while
other remote platforms continue. The private backend reuses those CLI bytes;
it is not compiled again locally. Pinned macOS targets are installed as needed.
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
5. Generate/sign `desktop-latest.json`, recalculate final checksums, sign checksum
   manifests with the existing CLI release root, then upload and publish together.

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

Ordinary Desktop push/PR jobs run clippy and tests on all four native platforms,
without optimized builds or installer generation. New pushes cancel superseded
Desktop runs, and Rust caches are saved on failure. For an isolated Windows test
installer, use the Desktop workflow's manual Run workflow entry, select a target
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
