#!/bin/sh
# DEVcode installer/updater: detects the platform and installs (or updates)
# the matching binary from the DEVcode GitLab release packages.
#
# The repository is private, so downloads need a gitlab.com token with
# read_api (read_package_registry is included in read_api for generic
# packages). Pass it via GITLAB_TOKEN, or glab's stored token is used
# when glab is installed and logged in.
#
# One-liner (fetch from the repo, run with the token in the environment):
#   curl -fsSL -H "PRIVATE-TOKEN: $GITLAB_TOKEN" \
#     https://gitlab.com/bitzzdev/DEVcode/-/raw/dev/install.sh | sh -s --
#
# Usage: install.sh [version]     default: latest release
set -eu

PROJECT_ID="89064691"
GROUP="devcode"
BIN="DEVcode"

log() { printf '%s\n' "$*" >&2; }
die() { log "install.sh: $*"; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v sha256sum >/dev/null 2>&1 || die "sha256sum is required"

TOKEN="${GITLAB_TOKEN:-}"
if [ -z "$TOKEN" ] && command -v glab >/dev/null 2>&1; then
  TOKEN=$(glab auth status --show-token 2>&1 | grep -o 'glpat-[A-Za-z0-9._-]*' | head -n1)
fi
if [ -z "$TOKEN" ]; then
  printf 'GitLab token (read_api scope) for gitlab.com/bitzzdev/DEVcode: ' >&2
  IFS= read -r TOKEN
fi
[ -n "$TOKEN" ] || die "a GitLab token is required to download from the private DEVcode repository"
API="https://gitlab.com/api/v4/projects/$PROJECT_ID"
AUTH="PRIVATE-TOKEN: $TOKEN"

[ "$(uname -s)" = "Linux" ] || die "DEVcode publishes Linux builds only (detected: $(uname -s))"

case "$(uname -m)" in
  x86_64) arch="x64" ;;
  aarch64 | arm64) arch="arm64" ;;
  *) die "unsupported architecture: $(uname -m) (published: x64, arm64)" ;;
esac

if ldd --version 2>&1 | head -n1 | grep -qi musl; then
  libc="-musl"
else
  libc=""
fi

if [ "$arch" = "x64" ] && [ -r /proc/cpuinfo ] && ! grep -q ' avx2 ' /proc/cpuinfo; then
  baseline="-baseline"
else
  baseline=""
fi

TARGET="linux-$arch$baseline$libc"

if [ "$#" -ge 1 ]; then
  VERSION="$1"
else
  VERSION=$(curl -fsS -H "$AUTH" "$API/releases?per_page=1" |
    grep -o '"tag_name":"[^"]*"' | head -n1 | cut -d'"' -f4)
  [ -n "$VERSION" ] || die "no releases found under $API/releases"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

log "Downloading $BIN $VERSION ($TARGET)"
url="$API/packages/generic/$GROUP/$VERSION/$BIN-$TARGET"
curl -fsS -H "$AUTH" -o "$tmp/$BIN-$TARGET" "$url" ||
  { log "URL: $url"; die "download failed (is $VERSION published for $TARGET?)"; }
curl -fsS -H "$AUTH" -o "$tmp/sha256sums.txt" "$API/packages/generic/$GROUP/$VERSION/sha256sums.txt" ||
  die "checksum list missing for $VERSION"
(cd "$tmp" && grep " $BIN-$TARGET\$" sha256sums.txt | sha256sum -c -) ||
  die "checksum mismatch for $BIN-$TARGET"
chmod +x "$tmp/$BIN-$TARGET"

existing=$(command -v "$BIN" 2>/dev/null || true)
if [ -n "$existing" ]; then
  [ -w "$existing" ] || die "$existing exists but is not writable; set DEVCODE_INSTALL_DIR"
  DEST="$existing"
else
  DEST="${DEVCODE_INSTALL_DIR:-$HOME/.local/bin}/$BIN"
  mkdir -p "$(dirname "$DEST")"
fi
mv "$tmp/$BIN-$TARGET" "$DEST"

log "Installed: $DEST"
"$DEST" --version >&2
bin_dir=$(dirname "$DEST")
case ":$PATH:" in
  *":$bin_dir":*) ;;
  *) log "Note: $bin_dir is not on your PATH; add it to your shell profile." ;;
esac
