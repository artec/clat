# First live-model validation

Do not treat CLAT as dogfood-ready until both gates below pass using a
real provider configuration entered through `/model`.

## Gate 1 — real model text streaming

1. Start the local development build:

```bash
./target/debug/clat
```

2. Run `/model` and enter the real Model, Endpoint, and API Key supplied
   by your provider.
3. Send:

```text
只回答：CLAT_LIVE_MODEL_OK
```

Pass condition: CLAT receives and displays a streamed response from the
real model containing `CLAT_LIVE_MODEL_OK`.

## Gate 2 — real model → native tool → real model

In the same TUI session, send:

```text
请必须使用 list_files 查看当前项目根目录，然后告诉我有哪些文件。
```

Pass condition:

1. the real model requests `list_files`;
2. CLAT executes the native read tool inside the current project sandbox;
3. the tool result is returned to the model;
4. CLAT performs a subsequent model turn and displays the final answer.

Only after both gates pass should the project move on to the first real
repository dogfood run.
