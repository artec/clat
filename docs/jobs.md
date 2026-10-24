# Background jobs

CLAT can run non-interactive commands as Host-owned jobs. A job belongs to its original project and conversation. Closing a browser window or quitting the desktop client leaves it running. Stopping the Host requests cancellation and waits for process cleanup. A Host crash/restart marks unfinished jobs as interrupted with an unknown result; CLAT never replays their commands automatically.

The Agent uses `job_start`, `job_output`, `job_list` and `job_kill`. Starting or stopping a job uses the normal execution permission pipeline. A successful start is a registration, not a successful command result. Inspect the actual exit code, timeout/cancellation flags and any verification receipt. Ordinary `exec_command`/`write_stdin` processes retain their existing per-Run lifetime.

The workbench **Background jobs** panel lists the current project's jobs, reads retained output and explicitly stops a selected job. Job events and completion notifications retain the original conversation identity. Opening this panel never changes the selected conversation or its draft.

Terminal clients support `/jobs` (list), `/jobs output <id> [offset]`, `/jobs stop <id> --confirm`, and `/jobs retry <id> --confirm`. The corresponding CLI entry is `clat jobs ...`, connected to the local Host. CLI responses are structured JSON. The output offset is a byte cursor; pages preserve UTF-8 character boundaries. Human reads do not advance the Agent's independent cursor.

Output is transient, redacted and bounded to 256 KiB per retained job; incomplete lines are held until a newline or process exit so credentials split between polls are not exposed. Lines over 16 KiB are omitted with an explicit loss indication. A Host retains at most 16 active jobs and 128 job records across mounted projects (64 per project). Output or notification failures never change the observed process exit.

A failed terminal-fact save or resource cleanup remains visible and blocks automatic Host takeover and session archival. **Retry saving result** / `/jobs retry ... --confirm` retries the existing fact or cleanup; it never starts the command again. If recovery cannot read source facts, the job service reports unavailable rather than an empty list.

Automatic continuation uses a separate explicit grant with an instruction, model and finite budget. Use **Authorize completion follow-up** or terminal schedule management; otherwise completion only publishes the job result and notification. A child Job never inherits a grant. See [Scheduled work](schedules.md).
