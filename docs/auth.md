# Key resolution

Go models:

1. `$OPENCODE_API_KEY`
2. macOS Keychain — service `lulz`, account `opencode-go`
3. `~/.local/share/opencode/auth.json` (written by `opencode auth login` before v2)
4. `~/.local/share/opencode/opencode.db` — OpenCode v2 stores its login here;
   the OAuth access token works as a Go key (read via the `sqlite3` CLI)

`lulz auth` shows which one was used.

Zen free models need their own key — the Zen endpoint rejects the Go key
(`free tier can only be used in OpenCode`) — resolved as:

1. `$OPENCODE_ZEN_API_KEY`
2. `~/.local/share/opencode/auth.json` (`opencode` / `opencode-zen`)

An OpenCode v2 login token is *not* a Zen key — the free tier refuses it too.
Without a Zen key, the free ids Go also serves (`minimax-m3.1-flash-free`,
`beam-free`, `longcat-2.5-preview-free`) route through Go instead and are
labelled `[Go]`; the Zen-only ones stay listed as `[Zen]`.

Picking a `[Zen]` model without a Zen key stops before the harness starts,
with instructions, instead of failing inside it.

## Claude Code logged in with claude.ai

A logged-in Claude Code asks once whether to use a custom `ANTHROPIC_API_KEY`.
Answering "no" is remembered, and from then on it sends the claude.ai OAuth
token instead. The gateway rejects that with `401 Missing API key.`, so
`lulz launch claude` records the key as approved in `~/.claude.json` (or
`$CLAUDE_CONFIG_DIR/.claude.json`) before starting. This is the same entry
Claude Code writes when you answer "yes".
