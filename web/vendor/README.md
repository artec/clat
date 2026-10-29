# Offline development widgets

Pinned CodeMirror 6 and xterm.js packages are bundled into `dist/entry.js` and
`dist/entry.css`, embedded by the Rust Host. End users need no JavaScript runtime
or network downloads. `licenses/` retains dependency notices. No font files are
bundled: terminals use system fonts or fonts selected from the local device library.

To deliberately update the bundle: `bun install --frozen-lockfile --ignore-scripts`
then `bun run build` in this directory. Commit the lockfile, source, generated
bundle and corresponding notices together. Normal Cargo builds consume the checked-in
bundle and do not invoke npm/Bun. This feature adds no CI job or UI test.

The pinned Unicode11 addon uses xterm's proposed Unicode provider API, so the
terminal factory explicitly enables `allowProposedApi` before loading it. This
is a widget API compatibility option, not OS or Agent permission escalation.
