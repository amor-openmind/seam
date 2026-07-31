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

# Always the latest release. GitHub resolves `latest` itself, so the command carries no
# version and never goes stale — an earlier design put the version in the command and in an
# environment variable, which made a one-line join into something to keep in step by hand.
#
# Every machine on latest is also the state seam needs: versions must match across a fleet,
# and "everyone takes the newest" is a rule a person can actually follow.
BASE="https://github.com/$REPO/releases/latest/download"

HOME_DIR="${SEAM_HOME:-$HOME/.seam}"
BIN="$HOME_DIR/seam"
mkdir -p "$HOME_DIR"

# Quit whatever seam is already running, FIRST — before any download or file move.
# Overwriting a running signed binary on macOS can kill the process mid-page-in, and
# two seams racing a port is a mess this script should never create. The quit is a
# loopback request; a copy too old to answer is handled by the new seam's own takeover.
echo "seam: stopping any running seam…"
NOTE="$HOME/Library/Application Support/dev.seam.seam/ui-port"
if [ -f "$NOTE" ]; then
  UIPORT="$(head -1 "$NOTE" 2>/dev/null || true)"
  if [ -n "$UIPORT" ]; then
    curl -s -m 3 -X POST "http://127.0.0.1:$UIPORT/action/quit" >/dev/null 2>&1 || true
    sleep 1
  fi
fi

# Fetch every time: a few megabytes over a LAN is cheaper than reasoning about whether the
# copy on disk is still the newest, and re-running the command is how a person updates.
echo "seam: fetching the latest release…"
curl -fsSL "$BASE/$ASSET" -o "$BIN.part"
if true; then
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
fi
echo "seam: got $("$BIN" doctor 2>/dev/null | sed -n 's/.*seam  *\(v[0-9.]*\).*/\1/p' | head -1)"

echo "seam: starting…"
exec "$BIN" run --connect "$SERVER"
