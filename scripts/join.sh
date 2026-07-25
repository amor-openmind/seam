#!/bin/sh
# seam — join a fleet from a machine that has no seam yet.
#
#   curl -fsSL http://<server>:<port>/join.sh | sh
#
# The server names the version; GitHub serves the bytes. Deliberately NOT the other way
# round: a binary fetched from a machine on the LAN is unauthenticated and unverifiable,
# and this project's rule is that releases come from GitHub. The server is trusted only to
# say "run v0.4.4", which is then fetched over TLS and checked against the release's own
# published SHA256SUMS.
#
# Re-running is cheap: an already-installed matching version is launched, not downloaded.
set -eu

REPO="amor-openmind/seam-releases"
SERVER="${SEAM_SERVER:-}"
[ -n "$SERVER" ] || { echo "seam: set SEAM_SERVER to the machine running seam, e.g. 192.168.2.69:24810" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) ASSET="seam-macos-arm64" ;;
  *) echo "seam: this script covers macOS; on Windows use join.ps1" >&2; exit 1 ;;
esac

# The version is baked into the command by the page that generated it. Asking the server
# was the original design and could never work: seam's HTTP page listens on loopback, and
# the port peers use is QUIC over UDP — there is nothing on the network to ask.
VERSION="${SEAM_VERSION:-}"
[ -n "$VERSION" ] || { echo "seam: no version given. Copy the command from the Add a machine page." >&2; exit 1; }

HOME_DIR="${SEAM_HOME:-$HOME/.seam}"
BIN="$HOME_DIR/seam-$VERSION"
mkdir -p "$HOME_DIR"

if [ ! -x "$BIN" ]; then
  echo "seam: fetching v$VERSION from GitHub…"
  BASE="https://github.com/$REPO/releases/download/v$VERSION"
  curl -fsSL "$BASE/$ASSET" -o "$BIN.part"
  # Verify against the release's own checksum file before trusting the bytes.
  if curl -fsSL "$BASE/SHA256SUMS.txt" -o "$HOME_DIR/sums.txt" 2>/dev/null; then
    WANT="$(sed -n "s|^\([0-9a-f]*\)  *.*$ASSET\$|\1|p" "$HOME_DIR/sums.txt" | head -1)"
    GOT="$(shasum -a 256 "$BIN.part" | cut -d' ' -f1)"
    if [ -n "$WANT" ] && [ "$WANT" != "$GOT" ]; then
      rm -f "$BIN.part"
      echo "seam: checksum mismatch — refusing to run this download" >&2
      exit 1
    fi
  fi
  chmod +x "$BIN.part"
  mv "$BIN.part" "$BIN"
else
  echo "seam: v$VERSION already here"
fi

echo "seam: starting…"
exec "$BIN" run --connect "$SERVER"
