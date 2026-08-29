# lulz

Run your coding-agent harness on your OpenCode Go subscription.

```
            Claude Code
           ╱
Codex ─── lulz ─── OpenCode Go
           ╲
            OpenCode
```

You keep the harness — its TUI, tools, `CLAUDE.md` / `AGENTS.md`, skills, MCP
servers, permissions, sandbox. Only the inference provider changes.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/Lulzx/lulz-router/main/install.sh | sh
```

Prebuilt for Apple Silicon; every other platform builds from source (needs
[Rust](https://rustup.rs)). Installs to `~/.local/bin` — override with
`LULZ_INSTALL_DIR`, pin a version with `LULZ_VERSION`.

Piping a script into a shell means running whatever that URL serves. Read it
first if you'd rather:

```sh
curl -fsSL https://raw.githubusercontent.com/Lulzx/lulz-router/main/install.sh -o install.sh
less install.sh && sh install.sh
```

Or from source directly:

```sh
cargo install --git https://github.com/Lulzx/lulz-router
```

One dependency (`serde_json`) — the bridge rewrites arbitrary user text between
two protocols, which is no place for a hand-rolled parser. Everything else is
std plus `curl` and `security`.

## Use

```sh
lulz launch claude                  # Claude Code on glm-5.3-flash
lulz launch claude -m minimax-m3
lulz launch codex -m gpt-5.6-luna
lulz launch opencode -m glm-5.3

lulz launch claude -- --resume      # everything after -- goes to the harness
lulz launch claude --print          # show the resolved env + argv, run nothing

lulz models                         # what your subscription exposes
lulz models --refresh               # re-read the gateway's model list
lulz default claude qwen3.8-max     # persist a per-harness default
lulz auth --save                    # stash the key in the macOS Keychain
```

Model aliases: `qwen`, `minimax`, `glm`, `kimi`, `gpt`/`luna`, `grok`,
`deepseek`, `mimo`, `hy`.

## How it works

`lulz` resolves your key, injects environment/config, and `exec()`s the harness
in place — so signals, terminal state, cwd and exit codes behave exactly as if
you had typed `claude` or `codex`.

**Claude Code** speaks the Anthropic Messages API, pointed at the gateway:

```sh
ANTHROPIC_BASE_URL=https://opencode.ai/zen/go
ANTHROPIC_API_KEY=<your go key>
ANTHROPIC_MODEL=glm-5.3-flash
ANTHROPIC_SMALL_FAST_MODEL=deepseek-v4-flash   # background/haiku traffic
CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000         # else it assumes 200k
```

`ANTHROPIC_API_KEY`, not `ANTHROPIC_AUTH_TOKEN` — the gateway's Messages
endpoint authenticates on `x-api-key` and 401s on `Authorization: Bearer`.

**Codex** gets an ephemeral provider on the command line:

```sh
codex -c model_provider="opencodego" \
      -c model_providers.opencodego.base_url="https://opencode.ai/zen/go/v1" \
      -c model_providers.opencodego.env_key="OPENCODE_API_KEY" \
      -c model_providers.opencodego.wire_api="responses" \
      -c model="gpt-5.6-luna"
```

## Capability matching

The gateway doesn't serve every model over every protocol, and — more subtly —
some models accept plain chat over Anthropic Messages but **400 on a tool
schema**, which makes them useless to a coding harness. `lulz` ships the probed
result:

| model | claude | codex | note |
|---|:--:|:--:|---|
| qwen3.5/3.6/3.7/3.8, minimax-m2.5/m2.7/m3, kimi-k3 | native | bridged | Messages+tools ok; no `/responses` |
| deepseek-v4-flash/pro, gpt-5.6-luna, muse-spark-1.2 | native | native | both |
| grok-4.5 | ❌ | native | Messages 401s for this model |
| glm-5…5.3, hy3, kimi-k2.x, mimo-\*, ox-alpha | ❌ | bridged | Messages rejects tool schemas |

The *roster* is never hardcoded: `lulz` reads the gateway's own
`/v1/models` and caches the ids at `~/.cache/lulz/models` for 12 hours, so a
model OpenCode shipped this morning shows up without a `lulz` release. The
table above only supplies what that endpoint doesn't report — protocol support
and context window. `--refresh` re-reads it on demand; if the fetch fails,
`lulz` falls back to the stale cache and, failing that, stops gating.

Naming a model the gateway doesn't serve is caught before the harness starts:

```
$ lulz launch claude -m glm-9-turbo
error `glm-9-turbo` isn't served by this gateway.
  close by: glm-5, glm-5.1, glm-5.2, glm-5.3, glm-5.3-flash
  full list: lulz models
```

`lulz doctor` re-probes each id on the live roster and caches the *capability*
result at `~/.cache/lulz/caps`, overlaying the baseline — so the table doesn't
rot as OpenCode adds models.

Only a verdict about the **model** is cached. A 200 is `ok` and a 400/404/422
is `no` (the model rejected the request shape), but 401, 403, 429, 5xx and a
dead connection are printed with their status code and left uncached — an
outage, a throttle or a bad key says nothing about what a model can do, and
recording it as `no` would gate a working model until the next probe.

## The bridge

Codex only speaks the Responses API — it dropped `wire_api = "chat"` in 0.14x —
and the gateway only serves `/responses` for the OpenAI-family models. So `lulz`
runs a translator in-process and points Codex at it:

```
codex ──Responses──▶ 127.0.0.1:<ephemeral> ──Chat Completions──▶ opencode.ai/zen/go
```

It engages automatically for any model the gateway won't serve natively, so
`lulz launch codex -m qwen3.8-max` just works. `--translate` forces it on;
`--no-translate` refuses instead of bridging.

Streaming is preserved end to end — upstream chunks are translated and flushed
as they arrive, so the TUI stays live. The translation is not cosmetic:

- **`developer` → `system`.** Codex writes its harness prompt as a `developer`
  message; Chat Completions doesn't know that role and upstream 400s the turn.
- **Fragmented tool calls.** The gateway sends a call's `id` and `name` in the
  first chunk and repeats `"id": ""` afterward. Clobbering it breaks the
  `call_id` that Codex matches tool output against.
- **Namespaced tool groups and `web_search` are dropped** — Chat Completions has
  one tool shape, a flat function. The core coding tools are plain functions and
  survive; MCP bundles do not (`LULZ_DEBUG=<file>` logs what was dropped).
- **A stream can open with an SSE comment.** minimax-m2.5 leads with
  `: keep-alive`. Treating the first non-`data:` line as an error body 502s
  every request before a token is read.
- **Tool-call indices don't start at zero.** minimax numbers its calls from 1.
  Buffering them positionally invents an empty call at index 0, which the
  harness reports as `unsupported call:` and upstream then rejects with
  `tool call id is invalid`. Calls are keyed by index, nameless entries are
  dropped, and a call with no id gets a synthesized one.
- **Parallel tool calls belong to one turn.** Codex emits each call as its own
  `function_call` item; one assistant message per call splits the results from
  their calls and strict providers reject the turn outright — minimax-m2.7
  answers `tool call result does not follow tool call`.
- **Inline `<think>` tags.** minimax streams its reasoning inside `content`
  rather than `reasoning_content`; without splitting it out, the harness renders
  the tags as the answer. The splitter holds back partial tags across chunk
  boundaries.
- **No reasoning items.** `reasoning_content` is forwarded as
  `response.reasoning_summary_text.delta` so you see the model think, but no
  reasoning *item* is emitted — Codex asks for `reasoning.encrypted_content`,
  which we can't produce, and would replay it on the next turn.

Because the gateway's Messages endpoint is what rejects tool schemas, the same
bridge in reverse (Messages → Chat) would unlock glm / hy3 / mimo / kimi-k2.x
for Claude Code too. Same idea, other direction — not built yet.

Debugging: `LULZ_DEBUG=/tmp/bridge.log lulz launch codex -m glm-5.3` mirrors
every request, translated body, and emitted event into the log.

## Verified

End-to-end through the real harnesses, not just curl:

```
claude -p   qwen3.8-max  minimax-m3  kimi-k3  deepseek-v4-pro  gpt-5.6-luna  → LULZ OK
codex exec  gpt-5.6-luna                                            (native) → LULZ OK
codex exec  qwen3.8-max  glm-5.3  kimi-k3  minimax-m3  deepseek-v4-pro
                                                                   (bridged) → tool call → answer
```

Every bridged run is a full agent turn: Codex called `exec_command`, ran
`cat marker.txt`, fed the output back, and answered from it. `glm-5.3` is the
one worth noting — it was blocked in *both* harnesses before the bridge.

The bridged run is the real test: Codex called `exec_command`, ran
`cat marker.txt`, fed the output back, and answered from it.

## Key resolution

1. `$OPENCODE_API_KEY`
2. macOS Keychain — service `lulz`, account `opencode-go`
3. `~/.local/share/opencode/auth.json` (written by `opencode auth login`)

`lulz auth` shows which one was used.
