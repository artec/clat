# CLAT Project Constitution

CLAT is a fast, local-first, open-source command line agent runtime.

All contributors and coding agents working in this repository should preserve these principles unless a deliberate project decision changes them.

1. **Local First** — Prefer local execution and local state when the task can be completed without a remote service.
2. **One Binary** — The core CLI should not require users to install Node.js, Python, or another language runtime.
3. **Model Agnostic** — Model vendors are adapters behind CLAT-owned interfaces; no provider defines the core runtime.
4. **MCP Native** — MCP is a first-class protocol for external capabilities, while native core tools may remain direct Rust implementations.
5. **Project Aware** — CLAT should understand the repository and working context it operates in.
6. **Permission First** — Side-effecting operations must pass through an explicit permission model.
7. **Dogfood Driven** — CLAT is developed against real work first, beginning with the ECAR repository and CLAT itself.
8. **Generalize, Never Special-case** — Product requirements may originate from ECAR, but CLAT core must contain reusable abstractions rather than ECAR-specific behavior.

## Initial engineering constraints

- Rust is the implementation language for the core runtime and CLI.
- Keep dependencies minimal and justified.
- Prefer standard, interoperable formats and protocols over project-specific equivalents.
- Keep the Agent Runtime, Model Provider, Tool, Permission, Context, Session, Project, and Event concepts separable.
- Favor observable event-driven execution so future CLI, TUI, IDE, desktop, or remote clients can consume the same runtime events.
- Do not add multi-agent complexity before the single-agent runtime is useful for real development work.
