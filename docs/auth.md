# Key resolution

Go models:

1. `$OPENCODE_API_KEY`
2. macOS Keychain — service `lulz`, account `opencode-go`
3. `~/.local/share/opencode/auth.json` (written by `opencode auth login`)

`lulz auth` shows which one was used.

Zen free models need their own key — the Zen endpoint rejects the Go key
(`free tier can only be used in OpenCode`) — resolved as:

1. `$OPENCODE_ZEN_API_KEY`
2. `~/.local/share/opencode/auth.json` (`opencode` / `opencode-zen`)

Picking a `[Zen]` model without a Zen key stops before the harness starts,
with instructions, instead of failing inside it.

## Claude Code logged in with claude.ai

A logged-in Claude Code asks once whether to use a custom `ANTHROPIC_API_KEY`.
Answering "no" is remembered, and from then on it sends the claude.ai OAuth
token instead. The gateway rejects that with `401 Missing API key.`, so
`lulz launch claude` records the key as approved in `~/.claude.json` (or
`$CLAUDE_CONFIG_DIR/.claude.json`) before starting. This is the same entry
Claude Code writes when you answer "yes".
