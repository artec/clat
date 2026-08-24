# Release signing

This document is for the repository owner publishing CLAT releases. It defines
the artifact trust chain, offline signing procedure, rollback behavior, key
rotation, and platform baseline.

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

- a POSIX shell with Bash and the normal repository build toolchain;
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
The script checks command/key/workflow presence and performs browser login when
the GitHub CLI credential store is empty.

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

- Update the Cargo/package versions and public documentation.
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

1. locates the draft release;
2. derives the complete expected asset set from the workflow matrix;
3. refuses to continue while an archive or manifest is missing;
4. downloads manifests;
5. signs and locally verifies each one;
6. uploads signatures;
7. publishes the release only after all signatures succeed.

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
