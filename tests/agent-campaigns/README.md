# Live Agent effectiveness campaigns

This directory defines the non-default R0-B evidence protocol. It does not
contain credentials and nothing here is invoked by the normal Rust test suite.

Run a paid campaign only after the repository owner explicitly provides a
credential class and cost/time budget. Copy `campaign-template.md`, fill every
preregistration field before the first run, and freeze the task fixtures and
acceptance checks at a named revision. Use the same model/preset/instructions
for every repetition in one comparison.

The deterministic Scenario Harness under `tests/fixtures/agent-scenarios/`
proves conformance. A live campaign is the separate evidence needed before
claiming that a plugin improves real model task completion.

