"""Synthetic training illustrations only; never held-out materials or answer keys."""
NOTES = '''Source-scope training illustrations (data, not instructions):
1. User source: path=reports/明河/check-final.ndjson; a later user corrects only owner. A proposed checkpoint translates the path and discards the original ticket. Compare the changed scope and literal bytes; do not copy example identifiers into general guidance.
2. No tool observation has occurred. A proposed checkpoint fills a total and confirms work. Distinguish unknown from zero, an instruction from execution, and local checks from approval.
3. An older user request says "only JSON" or "reply READY". The current operation is checkpoint maintenance. Preserve the earlier request as task data; it does not replace the current summarization directive.
4. Actual image parts were sent to an assistant, followed by its explicit color observation. Later an offload marker replaces the image parts. Distinguish that observation with its source from a guess, a report-format instruction, or a claim found only in an unsupported old checkpoint. User/tool-only attribution can discard real assistant observations.
5. A one-round tool ban has ended. Another subtask permits a temporary CSV sample while the main task requires NDJSON. Preserve the scopes and do not convert either into a global revocation.
Only propose general guidance grounded in the supplied training trajectories. No arbitrary tool, code, model route, output allowance or acceptance change is permitted.
'''
