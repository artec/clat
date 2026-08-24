# CLAT Live Agent Campaign

## Preregistration

- Campaign ID:
- Task-set revision:
- CLAT baseline revision:
- CLAT candidate revision:
- Date/platform:
- Provider/preset/model ID:
- Endpoint host:
- System/project instruction digests:
- Sampling/reasoning parameters:
- Credential class (never the secret):
- Maximum paid tokens:
- Maximum wall time:
- Repetitions per task:
- Primary metric:
- Minimum improvement:
- Allowed regressions:
- Safety guardrails:
- Stop conditions:

## Tasks and acceptance

For every task, name the immutable fixture and the verifier or prewritten
acceptance checks. Model self-report is not an acceptance oracle.

| Task | Fixture/revision | Acceptance oracle | Repetitions |
|---|---|---|---:|
|  |  |  |  |

## Results

Report every repetition, then the distribution. Do not retain only the best
run. Keep baseline and candidate model configuration identical unless the
campaign explicitly studies configuration.

| Task | Revision | Run | Success | Tool calls | Repeated calls | Approvals | Tokens | Wall time | Recovery |
|---|---|---:|---|---:|---:|---:|---:|---:|---|
|  |  |  |  |  |  |  |  |  |  |

## Guardrail verdict

- Unauthorized side effect: pass/fail
- Residual process/file: pass/fail
- Live/replay semantic difference: pass/fail
- Budget/stop condition honored: pass/fail
- Sanitized notes:

Any safety guardrail failure prevents graduation regardless of success rate,
latency, token, or tool-call improvements.

