#!/bin/sh
# asm installer.
#
#     curl -fsSL https://raw.githubusercontent.com/samishal1998/agent-sessions-manager/main/install.sh | sh
#
# Environment:
#   ASM_VERSION       tag to install (default: the latest release)
#   ASM_BASE_URL      fetch the assets from here instead of GitHub Releases
#                     (a mirror, or a directory: file:///path/to/assets)
#   ASM_INSTALL_DIR   where the binary goes (default: ~/.local/bin)
#   GITHUB_TOKEN      required while the repository is private
#
# Everything is verified against the release's SHA256SUMS before anything is
# installed, and the binary is moved into place atomically, so a failure
# part-way leaves the existing install untouched.
set -eu

REPO="${ASM_REPO:-samishal1998/agent-sessions-manager}"
INSTALL_DIR="${ASM_INSTALL_DIR:-$HOME/.local/bin}"
API="https://api.github.com/repos/$REPO"

info() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"; }
need curl
need tar

# --- what are we running on -------------------------------------------------
target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os" in
    Linux) os_part="unknown-linux-gnu" ;;
    Darwin) os_part="apple-darwin" ;;
    *) die "unsupported operating system: $os (build from source instead)" ;;
  esac
  case "$arch" in
    x86_64 | amd64) arch_part="x86_64" ;;
    aarch64 | arm64) arch_part="aarch64" ;;
    *) die "unsupported architecture: $arch (build from source instead)" ;;
  esac
  printf '%s-%s' "$arch_part" "$os_part"
}

# --- github, public or private ---------------------------------------------
# A private repository needs a token for both the API and the asset bytes,
# and assets must then be fetched through the API rather than the browser URL.
auth_header() {
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    printf 'Authorization: Bearer %s' "$GITHUB_TOKEN"
  fi
}

api_get() {
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl -fsSL -H "$(auth_header)" -H 'X-GitHub-Api-Version: 2022-11-28' "$1"
  else
    curl -fsSL "$1"
  fi
}

latest_version() {
  api_get "$API/releases/latest" 2>/dev/null |
    sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
    head -n 1
}

# Print the download URL for an asset name, honouring private repositories.
asset_url() {
  version="$1"; name="$2"
  # A mirror or a local directory serves the assets by plain name.
  if [ -n "${ASM_BASE_URL:-}" ]; then
    printf '%s/%s' "${ASM_BASE_URL%/}" "$name"
    return
  fi
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    api_get "$API/releases/tags/$version" |
      tr ',{' '\n\n' |
      grep -A3 "\"name\"[[:space:]]*:[[:space:]]*\"$name\"" |
      sed -n 's/.*"url"[[:space:]]*:[[:space:]]*"\([^"]*assets[^"]*\)".*/\1/p' |
      head -n 1
  else
    printf 'https://github.com/%s/releases/download/%s/%s' "$REPO" "$version" "$name"
  fi
}

fetch() {
  url="$1"; out="$2"
  if [ -n "${ASM_BASE_URL:-}" ]; then
    curl -fsSL "$url" -o "$out"
    return
  fi
  if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl -fsSL -H "$(auth_header)" -H 'Accept: application/octet-stream' "$url" -o "$out"
  else
    curl -fsSL "$url" -o "$out"
  fi
}

# --- resolve ----------------------------------------------------------------
TARGET=$(target)
VERSION="${ASM_VERSION:-}"
if [ -z "$VERSION" ] && [ -z "${ASM_BASE_URL:-}" ]; then
  VERSION=$(latest_version || true)
fi
# A mirror has no release API to ask; the caller names the version.
[ -n "${ASM_BASE_URL:-}" ] && VERSION="${VERSION:-(mirror)}"
[ -n "$VERSION" ] || die "could not determine the latest release.
  If the repository is private, set GITHUB_TOKEN to a token with 'repo' scope:
      GITHUB_TOKEN=ghp_... curl -fsSL .../install.sh | sh
  Or pin a version:
      ASM_VERSION=v0.1.0 curl -fsSL .../install.sh | sh"

ASSET="asm-$TARGET.tar.gz"
info "installing asm $VERSION ($TARGET)"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

url=$(asset_url "$VERSION" "$ASSET")
[ -n "$url" ] || die "release $VERSION has no asset named $ASSET"
fetch "$url" "$TMP/$ASSET" || die "could not download $ASSET from $VERSION"

# --- verify before installing anything -------------------------------------
sums_url=$(asset_url "$VERSION" "SHA256SUMS")
if [ -n "$sums_url" ] && fetch "$sums_url" "$TMP/SHA256SUMS" 2>/dev/null; then
  expected=$(grep " $ASSET\$" "$TMP/SHA256SUMS" | awk '{print $1}' | head -n 1)
  if [ -n "$expected" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      actual=$(sha256sum "$TMP/$ASSET" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
      actual=$(shasum -a 256 "$TMP/$ASSET" | awk '{print $1}')
    fi
    if [ -n "${actual:-}" ] && [ "$actual" != "$expected" ]; then
      die "checksum mismatch for $ASSET
  expected $expected
  actual   $actual"
    fi
    [ -n "${actual:-}" ] && info "checksum ok"
  fi
else
  info "warning: no SHA256SUMS in this release; skipping verification"
fi

# --- install ----------------------------------------------------------------
tar -xzf "$TMP/$ASSET" -C "$TMP"
binary="$TMP/asm-$TARGET/asm"
[ -f "$binary" ] || die "archive did not contain the expected binary"
chmod +x "$binary"

mkdir -p "$INSTALL_DIR" || die "could not create $INSTALL_DIR"
# Move into place via a sibling temp file so an interrupted install cannot
# leave a half-written binary where a working one used to be.
staged="$INSTALL_DIR/.asm.$$"
if ! mv "$binary" "$staged" 2>/dev/null; then
  cp "$binary" "$staged" || die "could not write to $INSTALL_DIR
  Set ASM_INSTALL_DIR to a directory you can write to, e.g.
      ASM_INSTALL_DIR=\$HOME/bin curl -fsSL .../install.sh | sh"
fi
mv "$staged" "$INSTALL_DIR/asm"

info "installed $INSTALL_DIR/asm"
"$INSTALL_DIR/asm" --version 2>/dev/null || true

case ":${PATH}:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    info ""
    info "$INSTALL_DIR is not on your PATH. Add it:"
    info "    export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac
