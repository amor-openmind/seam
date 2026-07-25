#!/bin/sh
# Issue a seam licence. OWNER ONLY — this needs the private key, which no build contains.
#
#   ./scripts/issue-licence.sh keygen                 # once: make the owner key pair
#   ./scripts/issue-licence.sh issue "Ali" 0          # perpetual
#   ./scripts/issue-licence.sh issue "Acme" 20999     # expires on that Unix day number
#
# Keep licence-private.pem OFF every machine that is not yours, and out of the repo.
# Build releases with the matching public key:
#   SEAM_LICENCE_KEY=$(cat licence-public.hex) cargo build --release
set -eu

case "${1:-}" in
  keygen)
    [ ! -f licence-private.pem ] || { echo "licence-private.pem already exists — refusing to overwrite the only copy" >&2; exit 1; }
    openssl genpkey -algorithm ed25519 -out licence-private.pem
    openssl pkey -in licence-private.pem -pubout -outform DER \
      | tail -c 32 | xxd -p -c 64 > licence-public.hex
    chmod 600 licence-private.pem
    echo "wrote licence-private.pem (keep this secret) and licence-public.hex"
    echo "build releases with: SEAM_LICENCE_KEY=\$(cat licence-public.hex) cargo build --release"
    ;;
  issue)
    NAME="${2:?usage: issue <name> <expiry-day-or-0>}"
    EXPIRY="${3:?usage: issue <name> <expiry-day-or-0>}"
    [ -f licence-private.pem ] || { echo "no licence-private.pem here — run keygen first" >&2; exit 1; }
    PAYLOAD="$NAME|$EXPIRY"
    printf '%s' "$PAYLOAD" > /tmp/seam-licence-payload
    SIG=$(openssl pkeyutl -sign -inkey licence-private.pem -rawin -in /tmp/seam-licence-payload | xxd -p -c 256)
    HEXPAY=$(printf '%s' "$PAYLOAD" | xxd -p -c 256)
    rm -f /tmp/seam-licence-payload
    echo "seam-$HEXPAY-$SIG"
    ;;
  *)
    echo "usage: $0 keygen | issue <name> <expiry-day-or-0>" >&2
    exit 1
    ;;
esac
