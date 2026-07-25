#!/usr/bin/env bash
# Build, sign and publish every release binary.
#
# The signing step is not optional on Apple Silicon: macOS SIGKILLs an arm64 binary whose
# code signature it cannot validate, and the message is a bare "killed" with no
# explanation. A binary that runs perfectly from ./target and dies after being downloaded
# is this, every time.
set -euo pipefail

TAG="${1:?usage: release.sh <tag>}"
cd "$(dirname "$0")/.."

export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar

cargo build --release --package seam-cli
cargo build --release --target x86_64-pc-windows-gnu --package seam-cli

mkdir -p dist
cp target/release/seam dist/seam-macos-arm64
cp target/x86_64-pc-windows-gnu/release/seam.exe dist/seam.exe

# Explicit ad-hoc signature. The linker's own signature does not reliably survive the
# round trip through a release download.
codesign --force --sign - dist/seam-macos-arm64
codesign --verify --verbose=1 dist/seam-macos-arm64

( cd dist && shasum -a 256 seam.exe seam-macos-arm64 > SHA256SUMS.txt )

gh release upload "$TAG" dist/seam.exe dist/seam-macos-arm64 dist/SHA256SUMS.txt --clobber

# Prove the published artifact actually runs, rather than trusting the upload.
tmp=$(mktemp)
curl -sL -o "$tmp" "https://github.com/amor-openmind/seam/releases/download/$TAG/seam-macos-arm64"
chmod +x "$tmp"
codesign --verify "$tmp"
"$tmp" --version
rm -f "$tmp"
echo "release $TAG published and verified"
