#!/bin/sh
# Install lulz. Prebuilt where one exists, from source otherwise.
#
#   curl -fsSL https://raw.githubusercontent.com/Lulzx/lulz-router/main/install.sh | sh
#
# Honours LULZ_VERSION (default: latest release) and LULZ_INSTALL_DIR
# (default: ~/.local/bin).
set -eu

REPO="Lulzx/lulz-router"
BIN="lulz"
INSTALL_DIR="${LULZ_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*" >&2; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

fetch() { # url -> stdout
    if have curl; then curl -fsSL "$1"
    elif have wget; then wget -qO- "$1"
    else die "need curl or wget"
    fi
}

download() { # url file
    if have curl; then curl -fsSL -o "$2" "$1"
    else wget -qO "$2" "$1"
    fi
}

target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os-$arch" in
        Darwin-arm64|Darwin-aarch64) echo "aarch64-apple-darwin" ;;
        Darwin-x86_64)               echo "x86_64-apple-darwin" ;;
        Linux-x86_64)                echo "x86_64-unknown-linux-gnu" ;;
        Linux-aarch64|Linux-arm64)   echo "aarch64-unknown-linux-gnu" ;;
        *) die "unsupported platform: $os $arch" ;;
    esac
}

from_source() {
    have cargo || die "no prebuilt binary for $TARGET, and cargo is not installed.
  install Rust from https://rustup.rs and re-run, or build it yourself:
  git clone https://github.com/$REPO && cd lulz-router && cargo install --path ."
    say "==> no prebuilt binary for $TARGET; building from source"
    # cargo appends /bin to --root, so build into a scratch root and place the
    # binary ourselves; INSTALL_DIR is a literal directory, not a prefix.
    root=$(mktemp -d)
    cargo install --git "https://github.com/$REPO" --root "$root" --force
    mkdir -p "$INSTALL_DIR"
    install -m 755 "$root/bin/$BIN" "$INSTALL_DIR/$BIN" 2>/dev/null \
        || { cp "$root/bin/$BIN" "$INSTALL_DIR/$BIN" && chmod 755 "$INSTALL_DIR/$BIN"; }
    rm -rf "$root"
    say "==> installed $INSTALL_DIR/$BIN"
    exit 0
}

TARGET=$(target)
VERSION="${LULZ_VERSION:-}"
if [ -z "$VERSION" ]; then
    VERSION=$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
    [ -n "$VERSION" ] || from_source
fi

ASSET="$BIN-$VERSION-$TARGET.tar.gz"
URL="https://github.com/$REPO/releases/download/$VERSION/$ASSET"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

say "==> fetching $BIN $VERSION for $TARGET"
download "$URL" "$TMP/$ASSET" 2>/dev/null || from_source

# Checksums are published alongside the asset; a tampered or truncated
# download should fail loudly rather than land on your PATH.
if download "$URL.sha256" "$TMP/$ASSET.sha256" 2>/dev/null; then
    want=$(cut -d' ' -f1 <"$TMP/$ASSET.sha256")
    if have shasum; then got=$(shasum -a 256 "$TMP/$ASSET" | cut -d' ' -f1)
    elif have sha256sum; then got=$(sha256sum "$TMP/$ASSET" | cut -d' ' -f1)
    else got=""; say "    (no shasum tool; skipping checksum)"
    fi
    [ -z "$got" ] || [ "$got" = "$want" ] || die "checksum mismatch
  expected $want
  got      $got"
else
    say "    (no published checksum; skipping)"
fi

tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -f "$TMP/$BIN" ] || die "archive did not contain $BIN"

mkdir -p "$INSTALL_DIR"
install -m 755 "$TMP/$BIN" "$INSTALL_DIR/$BIN" 2>/dev/null \
    || { cp "$TMP/$BIN" "$INSTALL_DIR/$BIN" && chmod 755 "$INSTALL_DIR/$BIN"; }

say "==> installed $INSTALL_DIR/$BIN"
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) say ""
       say "    $INSTALL_DIR is not on your PATH. Add it:"
       say "      fish:  fish_add_path $INSTALL_DIR"
       say "      bash:  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc"
       say "      zsh:   echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc" ;;
esac
say ""
say "    lulz launch claude"
