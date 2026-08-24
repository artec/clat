# Agent scenario fixtures

Each child directory is one deterministic, network-free Agent Scenario Harness
case. `scenario.json` scripts the model, `input/` is copied into an isolated
project, `expected/` is compared byte-for-byte after the run, and
`expected-report.json` pins the normalized gate result.

These fixtures exercise the real CLAT Application/plugin/permission/journal
path. A registered pre-fix failure may produce `gate: matched`; that means the
known baseline was reproduced, not that the missing capability is implemented.
When a later phase fixes the disease, update the same fixture deliberately and
explain the expected protocol/final-state change instead of deleting it.

Live-model effectiveness is separate and never runs in default CI. Before a
paid campaign, copy the preregistration block from
`docs/todo/agent-scenario-harness.md` and freeze its task-set revision, model,
budget, primary metric, repetitions, guardrails, and stop conditions.

