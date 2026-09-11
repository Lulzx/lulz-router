# Install

```sh
curl -fsSL https://raw.githubusercontent.com/Lulzx/lulz-router/main/install.sh | sh
```

Prebuilt for Apple Silicon; other platforms build from source (needs [Rust](https://rustup.rs)). Installs to `~/.local/bin` — override with `LULZ_INSTALL_DIR`, pin a version with `LULZ_VERSION`.

Prefer to read it first:

```sh
curl -fsSL https://raw.githubusercontent.com/Lulzx/lulz-router/main/install.sh -o install.sh
less install.sh && sh install.sh
```

Or from source:

```sh
cargo install --git https://github.com/Lulzx/lulz-router
```

## Updates

Once a day, an interactive `lulz` run asks GitHub for the latest release. That is one HEAD request, capped at 3 seconds. If a newer release exists, lulz downloads the tarball, checks it against the release's `.sha256`, confirms the new binary reports the right version, and swaps it in. It then re-runs the command you typed, now on the new version. If any step fails, you get a one-line notice and the command runs on the version you have. On platforms without a prebuilt binary, that notice is how you hear about a new release.

`lulz update` does the same thing on demand. Runs without a terminal (scripts, pipes) never update. Binaries built inside a cargo `target/` directory never replace themselves. Set `LULZ_NO_UPDATE=1` to turn updates off, for example to hold a version pinned with `LULZ_VERSION`.

One dependency (`serde_json`) — the bridge rewrites arbitrary user text between two protocols, no place for a hand-rolled parser. Everything else is std plus `curl` and `security`.
