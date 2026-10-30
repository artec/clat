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

Terminals and the settings preview use the pinned Canvas renderer with custom
block/box glyphs, so adjacent block elements and box lines join at cell boundaries
even when the selected system font has glyph side bearings. Letter spacing remains
a text setting; no negative spacing or PTY output rewriting is needed. If Canvas
cannot initialize, xterm keeps its DOM renderer and logs the rendering fallback.

`canvas-compat.ts` adapts only the pinned Canvas addon's glyph scratch canvas:
it connects the hidden canvas to the document before creating its 2D context,
and removes it when the atlas is disposed. This avoids WKWebView's inconsistent
user-font lookup on detached canvases ([WebKit 316777](https://bugs.webkit.org/show_bug.cgi?id=316777)).
The build fails if the exact upstream patch sites change; review this adaptation
when upgrading xterm. Fonts still come from the device, with no bundled font data.
