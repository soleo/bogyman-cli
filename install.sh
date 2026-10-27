#!/bin/sh
# Install the latest bogyman release without a package manager.
#
#   curl -fsSL https://raw.githubusercontent.com/soleo/bogyman-cli/main/install.sh | sh
#
# Picks the tarball for this OS and CPU from the latest GitHub release, checks it
# against the release's SHA256SUMS, and puts `bogyman` in ~/.local/bin.
#
# Options, as environment variables:
#   BOGYMAN_VERSION      install this tag instead of the latest (e.g. v0.1.0)
#   BOGYMAN_INSTALL_DIR  directory for the binary (default: ~/.local/bin)
#
# Needs curl or wget, tar, and shasum or sha256sum. POSIX sh; no bash required.

set -eu

REPO="soleo/bogyman-cli"
BASE_URL="${BOGYMAN_DOWNLOAD_BASE:-https://github.com/$REPO/releases/download}"
INSTALL_DIR="${BOGYMAN_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'bogyman install: %s\n' "$*" >&2; exit 1; }

need() {
  command -v "$1" >/dev/null 2>&1
}

fetch() {
  # fetch <url> <dest>
  if need curl; then
    curl -fsSL --retry 3 -o "$2" "$1"
  elif need wget; then
    wget -q -O "$2" "$1"
  else
    die "curl or wget is required"
  fi
}

latest_tag() {
  # The /releases/latest page redirects to /releases/tag/<tag>; read the Location
  # header so the script never touches the rate-limited API.
  if need curl; then
    curl -fsSI "https://github.com/$REPO/releases/latest" \
      | tr -d '\r' | sed -n 's/^[Ll]ocation: .*\/tag\///p' | tail -1
  else
    wget -q -S --max-redirect=0 -O /dev/null "https://github.com/$REPO/releases/latest" 2>&1 \
      | sed -n 's/^ *Location: .*\/tag\///p' | tail -1
  fi
}

os="$(uname -s)"
arch="$(uname -m)"
case "$os:$arch" in
  Darwin:arm64|Darwin:aarch64) target="aarch64-apple-darwin" ;;
  Darwin:x86_64)               target="x86_64-apple-darwin" ;;
  Linux:x86_64|Linux:amd64)    target="x86_64-unknown-linux-gnu" ;;
  Linux:aarch64|Linux:arm64)   target="aarch64-unknown-linux-gnu" ;;
  *) die "no prebuilt binary for $os/$arch; build from source with: cargo install --git https://github.com/$REPO bogyman-cli" ;;
esac

need tar || die "tar is required"
if need shasum; then
  sum() { shasum -a 256 "$1" | cut -d' ' -f1; }
elif need sha256sum; then
  sum() { sha256sum "$1" | cut -d' ' -f1; }
else
  die "shasum or sha256sum is required"
fi

tag="${BOGYMAN_VERSION:-}"
if [ -z "$tag" ]; then
  tag="$(latest_tag || true)"
  [ -n "$tag" ] || die "could not find the latest release of $REPO (set BOGYMAN_VERSION=vX.Y.Z to pick one)"
fi
case "$tag" in v*) ;; *) tag="v$tag" ;; esac

asset="bogyman-$tag-$target.tar.gz"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/bogyman-install.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "bogyman $tag for $target"
fetch "$BASE_URL/$tag/$asset" "$tmp/$asset" || die "download failed: $BASE_URL/$tag/$asset"
fetch "$BASE_URL/$tag/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $BASE_URL/$tag/SHA256SUMS"

expected="$(grep " $asset\$" "$tmp/SHA256SUMS" | head -1 | cut -d' ' -f1)"
[ -n "$expected" ] || die "$asset is not listed in SHA256SUMS"
actual="$(sum "$tmp/$asset")"
[ "$actual" = "$expected" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"

tar -xzf "$tmp/$asset" -C "$tmp"
bin="$tmp/${asset%.tar.gz}/bogyman"
[ -f "$bin" ] || die "the tarball did not contain bogyman"

mkdir -p "$INSTALL_DIR"
install -m 755 "$bin" "$INSTALL_DIR/bogyman" 2>/dev/null \
  || die "could not write to $INSTALL_DIR (set BOGYMAN_INSTALL_DIR to a writable directory)"

say "installed $INSTALL_DIR/bogyman"
"$INSTALL_DIR/bogyman" --version

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    say ""
    say "$INSTALL_DIR is not on your PATH. Add it, for example:"
    say "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac
say ""
say "Next: bogyman install   # wires Claude Code and Codex hooks"
say "      bogyman doctor    # confirms events arrive"
