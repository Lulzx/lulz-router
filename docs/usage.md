# Usage

```sh
lulz                               # pick a harness, then pick a model
lulz launch                        # same as bare lulz
lulz launch claude                  # live searchable model picker
lulz launch claude -m deepseek-v4-pro
lulz launch codex -m gpt-5.6-luna
lulz launch codex-app -m gpt-5.6-luna
lulz launch opencode -m glm-5.3

lulz launch claude -- --resume      # everything after -- goes to the harness
lulz launch claude --print          # show the resolved env + argv, run nothing

lulz models                         # what your subscription exposes
lulz models --refresh               # re-read the gateway's model list
lulz default claude qwen3.8-max     # persist a per-harness default
lulz auth --save                    # stash the key in the macOS Keychain
```

Model aliases: `qwen`, `minimax`, `glm`, `kimi`, `gpt`/`luna`, `grok`, `deepseek`, `mimo`, `hy`.

Codex always talks to a local `lulz` endpoint: the schema guard for Responses-native models, the translator for everything else. Nothing about the harness changes — same tools, same MCP servers, same sandbox.

`codex-app` is the same route into Codex's desktop app rather than its TUI. `codex app` opens the window and returns immediately, so `lulz` stays in the foreground holding the local endpoint open; Ctrl-C closes the route. Defaults are per-harness, so `lulz default codex-app <model>` is separate from `lulz default codex <model>`.

Every bare interactive `lulz launch <harness>` fetches both live rosters and opens a picker. Zen free models show first labelled `[Zen]`; paid Zen models are never included. Go subscription models follow labelled `[Go]`. Start typing to fuzzy-filter (`q38m` finds `qwen3.8-max`), arrows to move, Enter to select. A saved default is initially highlighted. Passing `-m` skips the picker, keeping scripts non-interactive.

Zen's `/models` response has no prices, so `lulz` intersects its live roster with the official free lineup — retired models drop out without ever admitting a paid one.
