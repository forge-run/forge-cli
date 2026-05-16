#!/usr/bin/env sh
# install.sh — one-liner installer for the `forge` CLI.
#
#   curl -sSf https://raw.githubusercontent.com/forge-run/forge-cli/main/install.sh | sh
#
# Detects platform via `uname`, downloads the matching binary from
# the latest GitHub Release, verifies sha256, drops `forge` into
# ${FORGE_INSTALL_DIR:-$HOME/.local/bin}. Pinned to POSIX sh + the
# tools shipped on every Linux/macOS box so it survives running
# under busybox-sh / dash / zsh / etc.
#
# Env vars (all optional):
#   FORGE_CLI_VERSION  pin a specific tag like `v0.2.0`; default is
#                      `latest` resolved via the GH API
#   FORGE_INSTALL_DIR  install location; default $HOME/.local/bin

set -eu

REPO="forge-run/forge-cli"
INSTALL_DIR="${FORGE_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${FORGE_CLI_VERSION:-latest}"

err() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

info() {
    printf '%s\n' "$*" >&2
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || err "missing required tool: $1"
}

require_tool curl
require_tool tar
require_tool uname
# shasum is on every macOS by default; on Linux it's part of perl
# (shipped on Debian/Ubuntu/Fedora). Fall back to sha256sum if that
# fails — Alpine ships only sha256sum, not shasum.
if command -v shasum >/dev/null 2>&1; then
    SHA256_CMD="shasum -a 256"
elif command -v sha256sum >/dev/null 2>&1; then
    SHA256_CMD="sha256sum"
else
    err "need shasum (macOS) or sha256sum (Linux) to verify the download"
fi

# Map uname output to release-archive target tuple. We only ship
# binaries for the four targets the release workflow builds.
OS=$(uname -s)
ARCH=$(uname -m)
case "$OS" in
    Darwin)
        case "$ARCH" in
            arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
            x86_64) TARGET="x86_64-apple-darwin" ;;
            *) err "unsupported macOS arch: $ARCH" ;;
        esac
        ;;
    Linux)
        case "$ARCH" in
            aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
            x86_64|amd64) TARGET="x86_64-unknown-linux-gnu" ;;
            *) err "unsupported Linux arch: $ARCH" ;;
        esac
        ;;
    *)
        err "unsupported OS: $OS — build from source via 'cargo install --git https://github.com/$REPO --locked'"
        ;;
esac

# Resolve `latest` to a concrete tag via the GH API. Pinned tag
# (FORGE_CLI_VERSION) skips the API call.
if [ "$VERSION" = "latest" ]; then
    info "resolving latest release…"
    # Anonymous API call — 60/hour ratelimit per IP is fine for an
    # install path that fires once per laptop.
    API_URL="https://api.github.com/repos/$REPO/releases/latest"
    VERSION=$(curl -sSfL "$API_URL" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' |
        head -n1)
    [ -n "$VERSION" ] || err "couldn't resolve latest release from $API_URL"
fi

ARCHIVE="forge-${VERSION}-${TARGET}.tar.gz"
ARCHIVE_URL="https://github.com/$REPO/releases/download/$VERSION/$ARCHIVE"
CHECKSUMS_URL="https://github.com/$REPO/releases/download/$VERSION/checksums.sha256"

# Stage in a temp dir so a partial download / failed verify
# doesn't leave half-baked state in INSTALL_DIR.
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

info "downloading $ARCHIVE…"
curl -sSfL -o "$TMPDIR/$ARCHIVE" "$ARCHIVE_URL" \
    || err "download failed: $ARCHIVE_URL"

info "downloading checksums…"
curl -sSfL -o "$TMPDIR/checksums.sha256" "$CHECKSUMS_URL" \
    || err "checksums download failed: $CHECKSUMS_URL"

info "verifying sha256…"
# Pull just the line for our archive out of the combined
# checksums file, then re-run shasum in $TMPDIR so the relative
# path matches.
EXPECTED=$(grep " $ARCHIVE\$" "$TMPDIR/checksums.sha256" |
    awk '{print $1}')
[ -n "$EXPECTED" ] \
    || err "no checksum entry for $ARCHIVE in checksums.sha256"
ACTUAL=$(cd "$TMPDIR" && $SHA256_CMD "$ARCHIVE" | awk '{print $1}')
[ "$EXPECTED" = "$ACTUAL" ] \
    || err "sha256 mismatch — expected $EXPECTED, got $ACTUAL. Re-run install or report at https://github.com/$REPO/issues"

info "extracting…"
tar -C "$TMPDIR" -xzf "$TMPDIR/$ARCHIVE"
# The archive contains a single directory `forge-<tag>-<target>/`
# with `forge`, LICENSE-*, README.md inside.
EXTRACTED="$TMPDIR/forge-${VERSION}-${TARGET}/forge"
[ -x "$EXTRACTED" ] \
    || err "extracted binary not found at $EXTRACTED — archive layout changed?"

mkdir -p "$INSTALL_DIR"
mv "$EXTRACTED" "$INSTALL_DIR/forge"
chmod 0755 "$INSTALL_DIR/forge"

info ""
info "installed $($INSTALL_DIR/forge --version)"
info "  binary:   $INSTALL_DIR/forge"
info ""

# PATH check — be friendly about it.
case ":$PATH:" in
    *:"$INSTALL_DIR":*)
        info "Run 'forge login' to get started."
        ;;
    *)
        info "Add $INSTALL_DIR to your PATH:"
        info ""
        info "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc   # or ~/.zshrc"
        info ""
        info "Then run 'forge login'."
        ;;
esac

# macOS unsigned-binary heads-up: Gatekeeper will quarantine the
# binary on first run. The right-click → Open dance bypasses it;
# Apple Developer ID signing is a future improvement.
if [ "$OS" = "Darwin" ]; then
    info ""
    info "macOS note: on first run, if you see 'cannot be opened because the"
    info "developer cannot be verified', remove the quarantine flag with:"
    info "  xattr -d com.apple.quarantine $INSTALL_DIR/forge"
fi
