# Changelog

## 0.1.0-rc.3 — official web quartet recipe (2026-10-03)

- Rebuilds distribution files before packing or publishing.
- Allows an unchanged static web service to own the empty web leaf slot, while
  protecting adapter host services and retaining lease cleanup.
- Adds single-scope `tools.get`, pinned `systemPrompt.getSectionOrder`, and an
  explicitly absent DSH `agents.currentInitiator` (no writable DSH journal).
- Adds the locked official 0.2.0-rc.2 web quartet recipe, network-free fixtures,
  CLAT permission/cancellation acceptance and a standalone package build.
- Reuses the author bundler's static package-metadata handling for the recipe;
  its executable disables `.env` and bunfig autoloading and is smoke-tested.

## 0.1.0-rc.2 — DSH 0.2.0-rc.2 alignment

- Preserves `deferLoading: true` in system-prompt tool schemas.
- Rejects direct timed-question requests with an actionable error; MCP ports
  use `tool-ask-user` legacy mode.
- Classifies `userQuestions.askTimed` as unsupported in semantic scans.
- Pins the 12-package cohort to DSH `dsh-v0.2.0-rc.2`
  (`639ed015397290b3745d163aafe02ffee4aa3f84`) and replaces the old
  DeepSeek transport entry with its API-key registration plugin.
- Updates the real Exa npm acceptance fixture to `0.2.0-rc.2`.

## 0.1.0-rc.1 — first public preview

- Publishes the author-side DSH-to-MCP adapter and porting commands under
  `@artec/clat-dsh-adapter` with the `next` npm tag.
- Pins the official 12-package compatibility cohort to DSH
  `dsh-v0.1.5-rc.3` (`a4c74a91e06b00fe0b0937bde982170c526cc842`).
- Preserves literal system-prompt sections marked `interpolate: false`,
  introduced after the previous DSH API pin.
- Starts `clat-dsh` correctly through npm's executable symlink and rejects
  tool content callbacks that the MCP bridge cannot preserve.
- Includes the MIT license in the npm package. The adapter remains an
  experimental bridge for portable leaf-plugin capabilities; host-spine and
  UI services are outside its compatibility promise.

The published DSH Exa plugin at `0.1.7-alpha.2` has also passed an isolated,
network-free MCP mount smoke test. This does not certify all alpha plugins.
