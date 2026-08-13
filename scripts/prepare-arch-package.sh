#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 1 ]] || { echo "usage: $0 OUTPUT_DIRECTORY" >&2; exit 2; }
output=$1
[[ $output == /* ]] || { echo "OUTPUT_DIRECTORY must be absolute" >&2; exit 2; }
mkdir -p -- "$output"
output=$(cd -- "$output" && pwd -P)
[[ $output != / ]] || { echo "refusing to write into /" >&2; exit 2; }

workspace=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$workspace/Cargo.toml" | head -n 1)
[[ -n $version ]] || { echo "workspace version is unavailable" >&2; exit 1; }
archive="$output/localdesk-$version-x86_64.tar.zst"
stage=$(mktemp -d)
trap 'gio trash "$stage" 2>/dev/null || true' EXIT
"$workspace/scripts/stage-release.sh" --build "$stage"
"$workspace/scripts/verify-release-stage.sh" "$stage"
tar --create --zstd --file "$archive" --directory "$stage" .

hash=$(sha256sum "$archive" | cut -d ' ' -f 1)
sed \
  -e "s/@VERSION@/$version/g" \
  -e "s/@SOURCE_SHA256@/$hash/g" \
  "$workspace/packaging/arch/PKGBUILD.in" > "$output/PKGBUILD"
echo "prepared $archive and $output/PKGBUILD"
