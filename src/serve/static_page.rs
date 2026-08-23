//! 静态面 v1：编译期常量欢迎页（§9）。rust-embed 是 PWA-4 引入点，
//! 一期不引（INV-S8 依赖零新增）。

pub(crate) const WELCOME_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>clat serve</title>
<style>
  body { font-family: ui-monospace, monospace; max-width: 40rem; margin: 4rem auto;
         padding: 0 1rem; color: #222; background: #fafafa; }
  h1 { font-size: 1.2rem; }
  code { background: #eee; padding: 0.1rem 0.3rem; border-radius: 3px; }
  pre { background: #eee; padding: 0.75rem; border-radius: 6px; overflow-x: auto; }
</style>
</head>
<body>
<h1>clat serve is running</h1>
<p>The machine API is served on this port:</p>
<pre>POST /api/session.list     -- list sessions
GET  /api/events           -- SSE event stream (replay + live)
POST /api/prompt.send      -- start a run (answer arrives on the stream)</pre>
<p>Every request must carry the token from the URL you opened
(<code>?t=…</code> or <code>Authorization: Bearer …</code>), and the
listener only accepts connections from this machine
(<code>127.0.0.1</code>).</p>
<p>A browser client is on the way (PWA-4). Until then, a quick check:</p>
<pre>curl -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:PORT/api/session.list</pre>
</body>
</html>
"#;
