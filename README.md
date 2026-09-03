# lulz

Run your coding-agent harness on your OpenCode Go subscription — plus OpenCode Zen's free models.

```
            Claude Code
           ╱
Codex ─── lulz ─── OpenCode Go (+ Zen free)
           ╲
            OpenCode
```

You keep the harness — TUI, tools, skills, MCP, sandbox. Only inference changes.

## Use

```sh
curl -fsSL https://raw.githubusercontent.com/Lulzx/lulz-router/main/install.sh | sh

lulz launch claude                  # live picker: [Zen] free models first, then [Go]
lulz launch claude -m minimax-m3
lulz launch codex -m gpt-5.6-luna
lulz launch claude -- --resume      # everything after -- goes to the harness
```

Two model sources: **Zen** (free, via `OPENCODE_ZEN_API_KEY`) and **Go** (subscription, via `OPENCODE_API_KEY`). The picker labels each row `[Zen]` or `[Go]`; `lulz` picks the endpoint from the chosen model.

## Docs

- [Install](docs/install.md) — script, source build, dependencies
- [Usage](docs/usage.md) — picker, aliases, defaults, `models`, `doctor`
- [How it works](docs/how-it-works.md) — env/config injection per harness
- [Capability matching](docs/capabilities.md) — probed protocol table, roster cache
- [The bridge](docs/bridge.md) — Responses ↔ Chat Completions translator
- [Verified](docs/verified.md) — end-to-end runs through the real harnesses
- [Key resolution](docs/auth.md) — where the key comes from
