# Verified

End-to-end through the real harnesses, not just curl:

```
claude -p   qwen3.8-max  minimax-m3  kimi-k3  deepseek-v4-pro  gpt-5.6-luna  → LULZ OK
codex exec  gpt-5.6-luna                                            (native) → LULZ OK
codex exec  qwen3.8-max  glm-5.3  kimi-k3  minimax-m3  deepseek-v4-pro
                                                                   (bridged) → tool call → answer
codex exec  muse-spark-1.3-contributor  with a recursive MCP tool schema
                                                       (native, guarded) → LULZ OK
```

Every bridged run is a full agent turn: Codex called `exec_command`, ran `cat marker.txt`, fed the output back, and answered from it. `glm-5.3` is the one worth noting — blocked in *both* harnesses before the bridge.

The guarded run is the recursive-schema regression: a local MCP server exposing one tool whose `inputSchema` is a `$ref` cycle (`node → children → node`). Straight to the gateway, Codex 400s the turn with `Error from provider (Console Go): ... Recursive JSON schemas are not currently supported`; through the guard the same turn answers.
