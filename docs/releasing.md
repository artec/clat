# Release signing

CLAT release archives are authenticated with Minisign. Each platform asset
has a one-entry `<asset>.sha256` manifest and a
`<asset>.sha256.minisig` signature. `clat upgrade` embeds
[`release/minisign.pub`](../release/minisign.pub) and fails closed when the
manifest, signature, asset name, digest, or release-tag binding is
missing or invalid. The signature's trusted comment (a signature-covered
field the publish script fills with `CLAT $TAG release checksum manifests`)
must match the release's tag, so replaying a genuinely-signed historical
asset under a newer tag — a signed rollback — is refused before install.
Replacement itself is stage-and-swap: the verified binary is first staged
into the install directory and only then swapped in, with a forced
rollback to the previous binary if a Windows two-step swap fails midway
(a failed swap never leaves the installation without an executable). The
first-install shell and PowerShell scripts intentionally remain dependency-free:
they use HTTPS plus a mandatory SHA-256 manifest and do not require Minisign.

Bootstrap boundary: an already-installed CLAT binary has an immutable embedded
trust root, but the default first-install path does not authenticate the
manifest with that key. An attacker who controls the GitHub account or can
replace the fetched script can therefore replace both the archive and its
SHA-256 manifest. Account-compromise-resistant first installation requires
obtaining the public key through an independent channel and manually verifying
the downloaded manifest with Minisign (or using a package manager whose own
signing chain supplies that trust root).

The current private key is generated locally at
`.release-secrets/clat-minisign.key`; that directory is Git-ignored. It must
remain offline from GitHub and CI. Before publishing a tag:

1. Back up the private key in the maintainers' encrypted credential store.
2. Confirm that `release/minisign.pub` matches the key pair with
   `minisign -R -s .release-secrets/clat-minisign.key -p /tmp/clat-release.pub`
   and compare the base64 key line.
3. Push the version tag. CI builds the platform assets and creates an unsigned
   **draft** release; draft releases are not returned by the latest-release
   endpoint used by installers and `clat upgrade`.
4. Confirm that the draft workflow ran from the intended tag commit and review
   the build before signing. A signature authenticates what the maintainer
   approved; it cannot turn an unreviewed build into trusted code.
5. From the maintainer machine, run `./publish`. If the GitHub CLI has no
   credential in the local credential store, the script opens the browser
   login automatically, copies GitHub's one-time device code to the clipboard,
   and reuses that login on later runs. It deliberately ignores `GH_TOKEN` and
   `GITHUB_TOKEN` only inside its own child processes; it never deletes or
   changes those environment variables. The script finds the draft release on
   GitHub, derives the complete expected asset set from the target matrix in
   `.github/workflows/release.yml`, and stops if any archive or SHA-256
   manifest is still missing. It then downloads the manifests, signs and
   locally verifies each one, uploads the signatures, and publishes the
   release as the final step.

The release workflow has no signing secret. After the offline signing step,
compromise of the GitHub account, release assets, or CDN can replace archives
and manifests, but cannot create a replacement signature accepted by the
public key embedded in existing CLAT binaries. Keep the private key out of
GitHub Actions even if an environment requires manual approval: that is still
the same administrative control plane.

Do not rotate the key by simply replacing the public key: already-installed
CLAT binaries would reject releases signed only by the new key. A rotation
must first ship a transition release that trusts both old and new public keys,
then switch signing keys, and remove the old key in a later release.

## Platform baseline

Release archives are built by
[`.github/workflows/release.yml`](../.github/workflows/release.yml):

| Asset | Build runner | Supported baseline |
|---|---|---|
| `aarch64-apple-darwin`, `x86_64-apple-darwin` | `macos-latest` | oldest macOS still targeted by the Rust toolchain |
| `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` | `windows-latest` | Windows 10+ |
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` (pinned) | glibc 2.39+ (Ubuntu 24.04 generation and newer) |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` (pinned) | glibc 2.39+ (Ubuntu 24.04 generation and newer) |

Linux policy: the runners are pinned to GitHub's lowest supported generation,
not `-latest`. Raising the baseline is a deliberate, documented decision that
happens only when the pinned runner is retired — never as silent drift of the
runner label. Users on older distributions can build from source: the tree
bundles SQLite and uses rustls, so a Rust toolchain is the only requirement.
