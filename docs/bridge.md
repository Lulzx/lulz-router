# The bridge

Codex only speaks Responses (it dropped `wire_api = "chat"` in 0.14x) and the gateway only serves `/responses` for OpenAI-family models. So `lulz` runs a translator in-process and points Codex at it:

```
codex ──Responses──▶ 127.0.0.1:<ephemeral> ──Chat Completions──▶ opencode.ai/zen/go
```

Engages automatically for any model the gateway won't serve natively — `lulz launch codex -m qwen3.8-max` just works. `--translate` forces it on; `--no-translate` refuses instead of bridging.

Streaming is preserved end to end — upstream chunks are translated and flushed as they arrive, so the TUI stays live. The translation is not cosmetic:

- **`developer` → `system`.** Codex writes its harness prompt as `developer`; Chat Completions 400s the turn.
- **Fragmented tool calls.** The gateway sends `id`+`name` in the first chunk, then `"id": ""`. Clobbering it breaks the `call_id` Codex matches tool output against.
- **Namespaced tool groups and `web_search` are dropped** — one flat function shape only. Core coding tools survive; MCP bundles do not (`LULZ_DEBUG=<file>` logs drops).
- **Streams can open with an SSE comment.** minimax-m2.5 leads with `: keep-alive`; treating the first non-`data:` line as an error 502s every request.
- **Tool-call indices don't start at zero.** minimax numbers from 1. Calls are keyed by index, nameless entries dropped, id-less calls get a synthesized id.
- **Parallel calls belong to one turn.** One assistant message per call splits results from calls; minimax-m2.7 answers `tool call result does not follow tool call`. Emitted as a single message.
- **Inline `<think>` tags.** minimax streams reasoning inside `content`; the splitter holds back partial tags across chunk boundaries.
- **No reasoning items.** `reasoning_content` forwards as `reasoning_summary_text.delta` for display only — Codex asks for `reasoning.encrypted_content`, which can't be produced.

For Responses-native models (Muse Spark 1.3), `lulz` bridges the other way: Claude Code sees a catalogued Sonnet-compatible model while the local bridge rewrites to the selected provider model and translates streaming text and tool calls back to Messages events.

The same composition makes Zen's free Chat Completions models available to both harnesses. `lulz` picks the Zen or Go endpoint from the chosen model (`opencode/<id>` vs `opencode-go/<id>`). `OPENCODE_ZEN_API_KEY` is preferred when set, else the existing OpenCode credential is reused.

Debug: `LULZ_DEBUG=/tmp/bridge.log lulz launch codex -m glm-5.3` mirrors every request, translated body, and emitted event into the log.
