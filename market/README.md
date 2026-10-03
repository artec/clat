# pi.at.cn deployment

`market/` is the standalone, zero-framework CLAT Plugin Index site. It is
independent of `clat serve`, contains no local credentials and can be deployed
to any static host that serves HTTPS.

## Preview or deploy the catalog

```bash
cd market
npm run validate
npm run build
```

Deploy the resulting `market/dist/` directory to `pi.at.cn`. The catalog is
usable without a machine index; every current record is deliberately marked
`preview`, so the site never presents an unpublished package as installable.

The included `_headers` file is understood by Cloudflare Pages and similar
hosts. On another host, reproduce its CSP, CORS and cache rules. In particular,
`catalog.json`, `index.json`, `index.json.minisig` and `packages/*` must be
readable cross-origin so the CLAT PWA and CLI can consume public data. Package
objects are immutable and may be cached for one year; index files must remain
short-cached.

## Publish a CLI-consumable signed index

The production trust anchor is `release/minisign.pub`. Keep its corresponding
secret key outside this repository. Update `index.source.json` and put
immutable `.clatpkg` objects under `market/packages/`, then run:

```bash
npm run build
npm run release-index -- --minisign-key /secure/path/to/minisign.key
```

`build` copies only `.clatpkg` objects into `dist/packages/`. `release-index`
recomputes every local artifact's byte length and SHA-256 before it gives the
index a seven-day validity and signs the exact trusted comment required by
CLAT. Re-run it before expiry, verify the generated files, then deploy all of
`dist/` atomically.

Publisher entries support parallel active/retired keys. A version identifies
the exact key that signed it. Rotation adds a new active key and retires the
old key; compromise marks the key `revoked` and adds package/artifact
revocations. Never rewrite an already published artifact URL or digest.
The complete onboarding, runtime-specific review and incident procedure is in
`PUBLISHING.md`.

## Add a package

1. Build and publisher-sign a normal `clat-plugin.json` package.
2. Run `clat plugin inspect <directory>` and `clat plugin pack <directory>
   --output <id>-<version>-<target>.clatpkg`.
3. Record the exact bundle byte length and SHA-256 in `index.source.json`.
4. Add the publisher/key record, version, compatibility, capability,
   dependency and artifact metadata.
5. Change the matching human catalog record to `available` only after the
   signed index and artifact are both ready.
6. Release-sign, test through `clat plugin market info/install --market` against
   a staging HTTPS origin, then deploy.

The market workspace includes a publisher helper for Rust/WASM or general MCP
packages. It writes the publisher record, computes CLAT's canonical inner-tree
message and asks the external `minisign` executable to sign it:

```bash
npm run sign-package -- \
  --package ../my-plugin \
  --publisher dev.example \
  --public-key /secure/path/publisher.pub \
  --minisign-key /secure/path/publisher.key
clat plugin inspect ../my-plugin
```

This is release tooling, not an end-user runtime dependency. The resulting
package remains consumable by the one CLAT binary.

No install hooks are supported. Search and info are read-only; package code is
not run until the local transactional install has committed and a later CLAT
runtime activates the package.

## First DSH capability bundle and repeatable package staging

`io.artec.dsh-official-web` bundles the four pinned official DSH web packages
as one native MCP executable. Start with `aarch64-apple-darwin`; an available
catalog card is not evidence that another platform has an artifact. Build and
verify each additional platform before adding its artifact record. End users
use the local PWA Install/Uninstall buttons; these build steps are for authors.

After building the recipe in `sdk/dsh-adapter/examples/official-web`, sign its
package using the reviewed publisher’s key. For the first artec release, the
owner selected `release/minisign.pub`; the public review record is
`publishers/artec.html`. The private key remains outside the repository. Then generate
artifact metadata without copying hashes by hand:

```bash
npm run stage-package -- \
  --package /absolute/signed-package-directory \
  --out /absolute/new-publication-directory \
  --target aarch64-apple-darwin \
  --publisher-key-id artec-2026 \
  --review-url https://pi.at.cn/publishers/artec.html \
  --source-url https://github.com/artec/clat \
  --clat /absolute/path/clat
```

This validates the publisher signature with CLAT, packs immutable bytes, and
writes `packages/`, `index.source.proposed.json` and `catalog.proposed.json`.
The proposal is **not publisher approval**. The owner verifies identity,
source/license, DSH revision, native-process risk and the review URL against
`PUBLISHING.md`, then merges the reviewed records into `index.source.json` and
`catalog.json`. Keep existing packages/publishers when merging. Combine further
platform artifacts under the same version; never overwrite an already published
version/target URL.

Copy the staged `.clatpkg` files into `market/packages/` before `npm run build`
(or directly into `dist/packages/` after building), then run the existing
`release-index` step with the owner-held CLAT signing key, and verify the index
with `release/minisign.pub` before deployment. Publisher signing keys and the
CLAT index signing key have distinct jobs; do not substitute a test key for the
embedded production trust anchor. Publish the immutable artifacts and matching
catalog/index/signature together. An available catalog entry must never precede
its signed install record. Serve `catalog.json` with the CORS headers in
`_headers`; a bare catalog URL that lacks those headers cannot be browsed from
the local PWA.

The index expires after seven days. Re-sign and deploy it before expiry even
when no package version changed. Expiry stops new installs/updates, while
already installed packages and offline local management continue to work.
Do not auto-disable packages because the index or network is unavailable.

The PWA host exposes five additive management RPCs documented in
`../docs/plugins.md`. Configuration schemas come from verified package trees,
not this site. In particular, annotate secret properties with `writeOnly: true`.
The static site must never request keys or local pairing tokens.

For the current owner-managed FTP deployment, follow [FTP-DEPLOY.md](FTP-DEPLOY.md).
