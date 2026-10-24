# Scheduled work and Job follow-up

CLAT's Host can execute explicitly authorized instructions in their original
conversation. Desktop and PWA provide **Scheduled work**; TUI uses `/schedule`,
and the CLI uses `clat schedule`. The same core service owns all rules.

Create a draft, then review its instruction, original project/session, trigger,
model route and budgets. **Authorize this exact task and budget** enables it.
Creating a draft or allowing a normal tool call does not authorize automatic
model execution. Changing the instruction, model or budget revokes the grant.
Pausing stops future admission; an admitted execution can still finish. Delete
the rule to remove future work. Explicitly cancel a running Agent separately.

Supported time rules are a delay, a specific time, a fixed interval, daily and
weekly. Daily/weekly rules retain an explicit IANA time zone. A daylight-saving
gap is skipped; a repeated local time fires only at the earlier instant. There
is no cron parser. The Host must be running: sleep or shutdown pauses execution.
On recovery, a one-shot rule is delivered once and a periodic rule catches only
its most recent due occurrence. Clock rollback does not replay an older one.

Every rule has a maximum execution count and total/per-execution token, time
and paid native Web-call limits. Tokens include input and output, with the
existing conservative reservation when a provider omits usage. Native DeepSeek
search/fetch share the explicit paid-call allowance; public HTTP fetch is free.
Automatic work disables subagent delegation, automatic title generation and
auxiliary plugin model sampling so those paths cannot silently spend outside the
grant. Ordinary interactive runs keep their existing behavior. Third-party tool
content and diagnostics remain original text; UI language does not change the
instruction submitted to the model.

Only one Agent Run executes per project. User runs and queued human input have
priority. A scheduled Run uses the normal model, permission, tool, context,
recorder and cleanup pipeline, with dependencies bound to its original session.
It does not select that session or consume the draft shown by another client.
Automatic approval/question cards are shown in Scheduled work, labeled with the
original session. They remain pending until answered, cancelled or timed out;
an automatic-execution grant never bypasses tool permission checks.

In **Background jobs**, choose **Authorize completion follow-up** to create a
separate grant. It applies to that specific Job only. Known success or failure
can trigger analysis; interrupted, explicitly killed, missing, unsaved or
uncleaned Jobs cannot. A Job started by the follow-up does not inherit its grant.
Closing a page or GUI does not cancel a Job or revoke a schedule.

The model tools `schedule_create`, `schedule_list`, `schedule_update` and
`schedule_delete` are scoped to the current actual session. Create/update only
produce drafts; model output contains no confirmation nonce. They cannot select
another project or enable a task. Subagents cannot create schedules.

## Terminal management

`clat schedule list` and `clat schedule deliveries` inspect the current project's
Host. `/schedule` has the same commands in the TUI. `create <JSON>` accepts a full
`ScheduleDraft`: `session`, `instruction`, `trigger`, `model_profile` (null for
the current route), and `budget`. `update <id> <revision> <JSON>` replaces it with
a draft and revokes authorization. For example:

```json
{
  "session": "SESSION-ID",
  "instruction": "Inspect the working tree and report pending changes.",
  "trigger": {"kind": "time", "rule": {"kind": "daily", "hour": 9, "minute": 0, "timezone": "Asia/Shanghai"}},
  "model_profile": null,
  "budget": {"max_runs": 5, "max_tokens": 500000, "max_seconds": 1500,
    "run_tokens": 100000, "run_seconds": 300,
    "max_paid_web_calls": 0, "run_paid_web_calls": 0}
}
```

`review <id> <revision>` prints the exact rule and a revision-bound nonce/digest.
After reading it, use `confirm <id> <revision> <nonce> <digest>`. These are user
management commands; do not put confirmation values into a model prompt.
`pause`, `resume`, and `delete` take the rule ID and revision. A changed model
configuration blocks dispatch and requires a new review. CLI commands do not
start a missing Host. Preferences and rule facts are shared, but UI language
remains client-local.

## Persistence and recovery

`schedules.json` is private, bounded, atomic Host control state; it stores rules,
grants, next times, reservations and delivery records. It stores no model API key.
Session journals remain the source of execution facts. Occurrences have stable
IDs and admission receipts. A reserved occurrence with proven absent admission
can resume after restart; a committed occurrence is never automatically rerun.
If its terminal journal fact exists, it supplies the recovered outcome; otherwise
the record becomes interrupted with unknown usage conservatively reserved.
There is no exactly-once promise for external side effects.

Corrupt state, external revisions and publication errors stop scheduling instead
of discarding rules. Preserve the file, repair it deliberately, then use
`reload --confirm`. Only settled delivery history is trimmed at the retention
limit. Pending/admitted work blocks Host takeover; future timers alone do not.
Sessions with rules require explicit removal of those rules before archival.

Local coverage uses fake models, isolated storage, real temporary child processes,
loopback Hosts and pure simulated-time calculations. It does not establish paid
provider, native Windows, or native desktop notification acceptance.
