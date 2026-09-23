# Changelog

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
