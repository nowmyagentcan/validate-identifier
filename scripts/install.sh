#!/bin/sh
# Installs validate-identifier @VERSION@ into ~/.local/bin after checking the archive's SHA-256.
# curl -fsSL https://github.com/nowmyagentcan/validate-identifier/releases/latest/download/install.sh | sh
set -eu
version=@VERSION@
base=${VALIDATE_IDENTIFIER_BASE:-https://github.com/nowmyagentcan/validate-identifier/releases/download/$version}
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=linux-x64 ;;
  Linux-aarch64 | Linux-arm64) target=linux-arm64 ;;
  Darwin-x86_64) target=macos-x64 ;;
  Darwin-arm64) target=macos-arm64 ;;
  *) echo "validate-identifier: no build for $(uname -s) $(uname -m)." >&2; exit 1 ;;
esac
archive=validate-identifier-$version-$target.tar.gz
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/$archive" "$base/$archive"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
expected=$(awk -v f="$archive" '$2 == f { print $1 }' "$tmp/SHA256SUMS")
if command -v sha256sum >/dev/null; then actual=$(sha256sum "$tmp/$archive"); else actual=$(shasum -a 256 "$tmp/$archive"); fi
if [ -z "$expected" ] || [ "${actual%% *}" != "$expected" ]; then
  echo "validate-identifier: the SHA-256 of $archive doesn't match SHA256SUMS; nothing was installed." >&2
  exit 1
fi
tar -xzf "$tmp/$archive" -C "$tmp" validate-identifier
# An empty file "runs" as an empty script, so the answer is checked too.
case $("$tmp/validate-identifier" --version 2>/dev/null || :) in
  "validate-identifier "*) ;;
  *) echo "validate-identifier: $archive has no validate-identifier that runs here; nothing was installed." >&2; exit 1 ;;
esac
dir=$HOME/.local/bin
mkdir -p "$dir"
mv "$tmp/validate-identifier" "$dir/validate-identifier"
"$dir/validate-identifier" --version
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Add $dir to your PATH: export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac
