#!/usr/bin/env sh
set -eu

REPO="${CODEUS_REPO:-atomicfield/codeus-cli}"
VERSION="${CODEUS_VERSION:-latest}"
INSTALL_DIR="${CODEUS_INSTALL_DIR:-$HOME/.local/bin}"
BIN_NAME="${CODEUS_BIN_NAME:-codeus}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: $1 is required" >&2
    exit 1
  }
}

detect_target() {
  os="$(uname -s)"
  arch="$(uname -m)"

  case "$os" in
    Darwin) os_part="apple-darwin" ;;
    Linux) os_part="unknown-linux-musl" ;;
    *) echo "error: unsupported OS: $os" >&2; exit 1 ;;
  esac

  case "$arch" in
    arm64|aarch64) arch_part="aarch64" ;;
    x86_64|amd64) arch_part="x86_64" ;;
    *) echo "error: unsupported architecture: $arch" >&2; exit 1 ;;
  esac

  printf '%s-%s' "$arch_part" "$os_part"
}

download_base() {
  if [ "$VERSION" = "latest" ]; then
    printf 'https://github.com/%s/releases/latest/download' "$REPO"
  else
    printf 'https://github.com/%s/releases/download/%s' "$REPO" "$VERSION"
  fi
}

verify_checksum() {
  archive="$1"
  checksums="$2"
  asset_name="$3"

  expected="$(grep "  $asset_name\$" "$checksums" | awk '{print $1}')"
  if [ -z "$expected" ]; then
    echo "error: checksum for $asset_name not found" >&2
    exit 1
  fi

  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$archive" | awk '{print $1}')"
  else
    actual="$(shasum -a 256 "$archive" | awk '{print $1}')"
  fi

  if [ "$actual" != "$expected" ]; then
    echo "error: checksum mismatch for $asset_name" >&2
    exit 1
  fi
}

need curl
need tar

target="$(detect_target)"
asset="codeus-$target.tar.gz"
base="$(download_base)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "Downloading $asset..."
curl -fsSL "$base/$asset" -o "$tmp/$asset"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
verify_checksum "$tmp/$asset" "$tmp/SHA256SUMS" "$asset"

mkdir -p "$INSTALL_DIR"
tar -xzf "$tmp/$asset" -C "$tmp"

candidate="$(find "$tmp" -type f -name 'codeus*' -perm -111 | head -n 1)"
if [ -z "$candidate" ]; then
  echo "error: archive did not contain a codeus executable" >&2
  exit 1
fi

cp "$candidate" "$INSTALL_DIR/$BIN_NAME"
chmod 755 "$INSTALL_DIR/$BIN_NAME"

echo "Installed $BIN_NAME to $INSTALL_DIR/$BIN_NAME"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "Add $INSTALL_DIR to PATH if $BIN_NAME is not found." ;;
esac
