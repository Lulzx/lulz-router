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

One dependency (`serde_json`) — the bridge rewrites arbitrary user text between two protocols, no place for a hand-rolled parser. Everything else is std plus `curl` and `security`.
