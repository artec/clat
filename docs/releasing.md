# Release signing

CLAT release archives are authenticated with Minisign. Each platform asset
has a one-entry `<asset>.sha256` manifest and a
`<asset>.sha256.minisig` signature. `clat upgrade` embeds
[`release/minisign.pub`](../release/minisign.pub) and fails closed when the
manifest, signature, asset name, or digest is missing or invalid. The
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
5. From the maintainer machine, run `./publish`. It finds the draft release on
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
