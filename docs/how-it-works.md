# How it works

`lulz` resolves your key, injects environment/config, and `exec()`s the harness in place — signals, terminal state, cwd and exit codes behave as if you'd typed `claude` or `codex` directly. When a bridge or the schema guard has to run inside the `lulz` process (Codex always, Claude Code on Responses-only models), it runs the harness as a child instead and mirrors its exit code, because `exec()` would wipe those threads out.

**Claude Code** speaks Anthropic Messages, pointed at the gateway:

```sh
ANTHROPIC_BASE_URL=https://opencode.ai/zen/go
ANTHROPIC_API_KEY=<your go key>
ANTHROPIC_MODEL=minimax-m3
ANTHROPIC_SMALL_FAST_MODEL=deepseek-v4-flash   # background/haiku traffic
CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000         # else it assumes 200k
```

`ANTHROPIC_API_KEY`, not `ANTHROPIC_AUTH_TOKEN` — the gateway's Messages endpoint authenticates on `x-api-key` and 401s on `Authorization: Bearer`.

**Codex** gets an ephemeral provider on the command line, pointed at lulz's local schema guard (native Responses models) or at the translator (everything else):

```sh
codex -c model_provider="opencodego" \
      -c model_providers.opencodego.base_url="http://127.0.0.1:<ephemeral>/v1" \
      -c model_providers.opencodego.env_key="OPENCODE_API_KEY" \
      -c model_providers.opencodego.wire_api="responses" \
      -c model="gpt-5.6-luna"
```

**Codex Desktop** (`codex-app`) is the same provider injection with the `app` subcommand in front — `codex app` takes the same `-c` overrides. It hands off to the desktop process and exits, so `lulz` parks in the foreground instead of tearing down the endpoint the window is still using.

Codex's own metadata does not know these slugs, so it prints `Model metadata for <id> not found` and runs on fallback metadata (a 272k context window). That catalog is Codex's, not something lulz can inject: `model_catalog_json` replaces Codex's catalog rather than extending it, and any entry added to it also defers every MCP tool behind Codex's `tool_search` — a worse trade than the warning.
