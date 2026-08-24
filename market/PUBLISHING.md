# CLAT Plugin Index publishing policy

This file is the review contract for records shipped by `pi.at.cn`. A pull
request may propose metadata, but only the repository owner signs and publishes
the production index.

## Publisher onboarding

A publisher request must include:

- a stable lowercase id and public identity/contact URL;
- one Minisign public key generated and stored independently of CLAT;
- proof that the requester controls the linked source repository;
- the package source revision, reproducible build instructions and license;
- an explicit runtime class: capability-bounded `wasm-component` or
  unrestricted out-of-process `mcp-stdio`;
- every capability, dependency, network/service dependency and data path;
- for DSH ports, the pinned DSH revision and the generated compatibility report.

The review record is linked as `reviewUrl`. Approval adds the publisher as
`trusted`; incomplete review stays absent, not optimistically `trusted`.

## Package review gates

Every version must pass the common gates:

1. source identity, license and version tag match the package manifest;
2. clean reproducible build in a fresh environment;
3. `clat plugin inspect` and deterministic double-pack produce the same bytes;
4. declared capabilities match observed behavior and documentation;
5. no install hooks, symlinks, generated credentials or embedded private keys;
6. dependency ranges are minimal and resolve without cycles or conflicts;
7. publisher signature, artifact length/SHA-256 and target compatibility match;
8. smoke test through a staging signed index and a clean CLAT storage root.

Additional WASM gates verify imported WIT interfaces, filesystem preopens,
fuel/memory behavior and permission-mode intersections. Additional MCP/DSH
gates treat the executable as arbitrary native code: audit subprocesses,
network destinations, inherited environment, shutdown/cancellation, tool
effects and the adapter's unsupported-seam report. A valid signature never
substitutes for those runtime-specific checks.

## Version state

- `available`: reviewed, signed, immutable artifact and signed index record are
  deployed together.
- `yanked`: not selected for new solutions, but bytes remain immutable for
  forensic and rollback reference.
- `revoked`: installation fails once `effectiveAtUnix` is reached. Use for
  compromise, malicious behavior or a materially false security declaration.
- vulnerability advisory: identifies an affected version range and severity;
  installation blocks unless the user explicitly accepts it.

Never reuse a version or artifact URL. A correction is a new version.

## Key rotation and incident response

Routine rotation:

1. add the new public key as `active` with a future `notBeforeUnix`;
2. publish one signed index containing both keys;
3. sign new versions with the new key;
4. change the old key to `retired` after its last legitimate publication.

Compromise:

1. mark the key `revoked` in `index.source.json`;
2. add version or artifact revocations for every uncertain release;
3. add advisories where installed versions need operator action;
4. generate, owner-sign and atomically deploy a fresh short-lived index;
5. publish a human incident note at the linked review/advisory URL.

Removing old bytes is not revocation: clients act on the signed record, and
immutable evidence remains useful. The production secret key for the market
index and all publisher secret keys are never committed to this repository.
