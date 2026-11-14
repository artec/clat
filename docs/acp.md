# ACP external Agents

CLAT runs locally configured Agent Client Protocol (ACP) v1 Agents over stdio. In **Settings → External Agents**, use **Add Agent** below the list or **Edit** on an existing row to open the form. Enter an absolute executable path, an argument array and optional Host environment variable **names**. Install and sign in to the Agent using its own instructions. On Windows, choose a directly executable launcher rather than a batch file. Trust the executable for each project before starting it. An executable or launch configuration change requires renewed trust.

- In the composer, choose **+ → Skills and extensions → External assistance**. Review the task and project directory before sending. The delegation card holds progress, the transcript and cancellation. When the Agent completes with a final answer, CLAT queues that answer and the confirmed task for the original conversation. The continuation follows that conversation's existing permissions and can be cancelled before admission.
- In the project sidebar, choose **New external session** to open a dedicated Agent conversation. **Settings → External Agents → External sessions** opens previous local transcripts. **Browse Agent history** lists sessions if the Agent advertises that capability. CLAT imports sessions from the listing for the current project. Closing a connection preserves local history. Deleting from the Agent requires a separate confirmation.
- In the standalone or Host-connected TUI, start with `/acp`. Use `/acp add JSON`, `/acp agents` and `/acp trust ID REVISION` to configure a project. Use `/acp delegate AGENT TASK`, `/acp new AGENT`, `/acp send SESSION TASK`, `/acp status ID` and `/acp cancel ID` for work. The detail panel refreshes while work is in progress. `/acp remote AGENT [CURSOR]` lists Agent history. Use `/acp import AGENT REMOTE_ID` with an ID from the current project's listing.

The external executable has the Host account's OS permissions. It may use its own credentials, tools and network. CLAT-mediated file operations are held to a project boundary and writes require approval. Plan Mode and Read Only disallow starting an external Agent from that CLAT session. See [Permissions](permissions.md) and the [ACP trust model](https://agentclientprotocol.com/protocol/v1/security-considerations).

## Capability coverage

| Surface | Current implementation |
| --- | --- |
| Transport | ACP v1 JSON-RPC over managed local stdio. |
| Session lifecycle | New, prompt, cancel, negotiated load/resume/list/close/delete. History remains locally viewable when the Agent cannot restore its context. |
| Authentication | Agent-handled authentication and negotiated logout. Terminal authentication is pending. |
| Content | Text in the composer and TUI. Web/PWA/Desktop dedicated external sessions can send supported raster image and audio attachments with a capable Agent. Host prompt actions also accept resource links and capability-gated embedded resources. User-supplied session instructions and additional canonical project subdirectories are available through Host session options. |
| Agent updates | Streaming messages, thought and tool updates, plans, commands, session metadata, notices, usage and compaction notifications are collected. Web/PWA/Desktop can show body text, image and audio blocks, resource references, plans and context usage. TUI shows text and references. |
| Client callbacks | Permission questions, bounded form interaction and project-fenced text reads/writes. The gated terminal callbacks are available on macOS through a project-read/temp-write and network-denied profile. URL-mode elicitation and hosted terminals on Windows/Linux are pending. |
| MCP sharing | Empty by default. Explicit session options can share credential-free HTTPS endpoints if the Agent advertises the chosen MCP transport. Managed credential-bearing and stdio server forwarding are pending. |
| History | External transcripts and pending continuations are stored in the Host's ACP catalog. The CLAT continuation writes through normal journal admission with an idempotent external-task source. |
| Deployment | User-installed local Agents. Registry installation, remote ACP, v2 draft and the ACP server role are outside this integration. |

ACP wire behaviors are documented in the [official v1 protocol](https://agentclientprotocol.com/protocol/v1/overview). See [Gemini CLI ACP mode](https://geminicli.com/docs/cli/acp-mode/) and the [Codex ACP adapter](https://github.com/agentclientprotocol/codex-acp) for external launch configurations.
