# Session history and timeline

Open **History timeline** in the workbench sidebar to browse recent conversations or search across them. Search defaults to the current project. Select **All trusted projects** to widen the scope. Results show the session, project, date and matching excerpt. An empty search lists sessions in the selected scope. Choose a hit to open its exact record.

The execution workbench has three timeline lanes for input, model activity and tools. **Sequence** shows recorded order. **Time** uses available time boundaries, marks estimated and unknown durations, and is unavailable when no duration can be calculated. Select a segment to open its record in the turn list. Turns start collapsed. Choosing a recent session shows the latest turn and its final reply when one was recorded. A search hit opens its turn automatically. Steering stays in its original turn, and compaction appears separately with the recorded range, record count and token estimate when available.

The timeline starts with the full session in view. Use **+** and **−** to zoom horizontally. The zoomed timeline follows the list as you scroll. Its closest zoom covers the visible rows, including all records represented by a visible collapsed turn. In **Time** mode, records without a timestamp appear in a separate strip at the right edge.

Use **Search this session** to find literal text in replies, thinking, tool arguments and results. The adjacent arrows step through matches. Select a row to inspect **Overview**, **Preview**, **Original** or **Source**. Original content and its copy action use the redacted event data. On narrow windows, close the details drawer to return to the same list position. Use the details button in the toolbar to reopen the selected record. **Locate in conversation** is available for supported records in the current project.

The workbench opens a read-only snapshot. Use **Refresh snapshot** to see newer records from a running conversation. Earlier and later pages use the same snapshot and preserve the reading position. A replacement or truncation that invalidates the snapshot asks you to refresh. Browsing leaves the active conversation and draft in place.

`clat history search <text>` and `/history search <text>` search the current project. Use `all <text>` instead of `search <text>` for all trusted projects. The TUI shows a result picker: use ↑/↓ and Enter to inspect a result. `events <reference JSON>` and `read <reference JSON>` accept a returned reference object containing `project`, `session`, and nullable `seq`. They do not resume or switch a session. CLI queries connect to the shared local Host.

Agent tools are `session_search`, `session_event_search`, and `session_event_read`. Their project identity comes from the mounted caller; arguments cannot grant cross-project access. Session search excludes the current session; explicit event search/read of the current session stops before the current model step. Read continuation uses the returned character offset and content version. Unknown, replaced, or out-of-scope references fail explicitly.

Cross-session search includes titles, visible user and assistant text, and readable tool observations. Its index excludes reasoning, system instructions, raw diagnostics and credential fields. Compaction retains original text that still exists in the journal; ordinary replacements hide old text, including derived fallback titles. A result is a historical observation, not evidence that the work still holds today.

The Host maintains a private disposable SQLite FTS5 cache under `history-index/` in the storage root. Unicode word and trigram indexes serve normal queries; short queries use bounded literal scans. Queries are parameterized, never evaluated as FTS operators or regular expressions. Each operation has a 30-second deadline, checks cancellation, and bounds candidates/pages. The model receives fewer results than the user interface; truncation requests a narrower query. Source revisions drive incremental reconciliation of appends, truncations, deletions and external writes. Corrupt derived databases are rebuilt; source read failures remain errors, never false empty results.

The event inspection commands list committed events with optional exact event-type filtering and pagination. JSON is redacted, bounded and version-pinned for continuation. The trajectory loads full content only when a record is selected. These reads use the query-only port and preserve the original journal.

## Transfer timelines with DeepSeek Harness

In CLAT **History**, select a project timeline and click **Export DSH ZIP** in the top toolbar. The archive uses the DSH 0.2.2 V4 layout for the session log, subagent logs and attachment objects. CLAT adapts the copied journal to DSH source ordering and tool lifecycle rules. A denied tool call keeps its recorded reason and is marked as unstarted in the copy. The archive can contain original prompts, tool output, paths and attachments. Review it before sharing it with someone else. If a referenced object is unavailable, the archive includes `attachments-missing.txt`.

To open a ZIP exported by DSH, go to the trusted project where you want to keep the copy and click **Import DSH ZIP**. The archive appears in **Imported timelines**. You can inspect its turns, search within a timeline, select subagent timelines, and view available images. Exporting an imported timeline downloads the original ZIP, including any files it contained. CLAT stores imported archives separately from runnable sessions. Reimporting the same ZIP in the same project reuses its existing entry. The compressed and expanded archive each have a 64 MiB limit.

To take a CLAT ZIP to DSH, prepare a fresh DSH data directory. The optional final path associates the copied logs with an existing project directory on the destination computer.

```sh
clat history unpack-dsh ./dsh-session-example.zip ./dsh-transfer /path/to/project
DSH_HOME="$PWD/dsh-transfer" dsh web
```

Open the corresponding project in DSH and select the imported session in its trajectory view. DSH 0.2.2 provides a ZIP export action. The CLAT command prepares the DSH log and attachment store for the reverse direction. Choose a new destination directory for each transfer and use archives from sources you trust when opening them as DSH sessions.
