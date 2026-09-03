# How it works

`lulz` resolves your key, injects environment/config, and `exec()`s the harness in place — signals, terminal state, cwd and exit codes behave as if you'd typed `claude` or `codex` directly.

**Claude Code** speaks Anthropic Messages, pointed at the gateway:

```sh
ANTHROPIC_BASE_URL=https://opencode.ai/zen/go
ANTHROPIC_API_KEY=<your go key>
ANTHROPIC_MODEL=minimax-m3
ANTHROPIC_SMALL_FAST_MODEL=deepseek-v4-flash   # background/haiku traffic
CLAUDE_CODE_MAX_CONTEXT_TOKENS=1000000         # else it assumes 200k
```

`ANTHROPIC_API_KEY`, not `ANTHROPIC_AUTH_TOKEN` — the gateway's Messages endpoint authenticates on `x-api-key` and 401s on `Authorization: Bearer`.

**Codex** gets an ephemeral provider on the command line:

```sh
codex -c model_provider="opencodego" \
      -c model_providers.opencodego.base_url="https://opencode.ai/zen/go/v1" \
      -c model_providers.opencodego.env_key="OPENCODE_API_KEY" \
      -c model_providers.opencodego.wire_api="responses" \
      -c model="gpt-5.6-luna"
```
