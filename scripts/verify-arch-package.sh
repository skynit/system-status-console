#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 1 ]] || { echo "usage: $0 PACKAGE" >&2; exit 2; }
package=$1
[[ $package == /* ]] || { echo "PACKAGE must be absolute" >&2; exit 2; }
[[ -f $package ]] || { echo "package not found: $package" >&2; exit 2; }

extract_dir=$(mktemp -d)
trap 'gio trash "$extract_dir" 2>/dev/null || true' EXIT
bsdtar -xf "$package" -C "$extract_dir"

(
  cd "$extract_dir"
  sha256sum -c usr/share/doc/localdesk/artifact-manifest.sha256
)
[[ -f $extract_dir/etc/xdg/autostart/dev.skynit.localdesk-daemon.desktop ]]
[[ -f $extract_dir/usr/share/applications/dev.skynit.localdesk.desktop ]]
[[ -f $extract_dir/usr/share/icons/hicolor/1024x1024/apps/dev.skynit.localdesk.png ]]
[[ -f $extract_dir/usr/share/licenses/localdesk/LICENSE ]]
desktop-file-validate \
  "$extract_dir/etc/xdg/autostart/dev.skynit.localdesk-daemon.desktop" \
  "$extract_dir/usr/share/applications/dev.skynit.localdesk.desktop"

if find "$extract_dir" -type f \( -name '*.service' -o -name '*.socket' \) -print -quit | grep -q .; then
  echo "package unexpectedly contains a systemd unit" >&2
  exit 1
fi
for file in "$extract_dir"/usr/lib/localdesk/*; do
  [[ $(stat -c '%a' "$file") == 755 ]] || {
    echo "unexpected packaged mode: $file" >&2
    exit 1
  }
  if getcap "$file" | grep -q .; then
    echo "package unexpectedly grants a file capability: $file" >&2
    exit 1
  fi
done

archive_binaries=$(bsdtar -tvf "$package" | awk '
  $3 == "root" && $4 == "root" && $1 == "-rwxr-xr-x" && $NF ~ /^usr\/lib\/localdesk\/localdesk-/ { count++ }
  END { print count + 0 }
')
[[ $archive_binaries == 6 ]] || {
  echo "package does not contain six root-owned mode-0755 binaries" >&2
  exit 1
}

pacman -Qlp "$package" | grep -Fxq 'localdesk /etc/xdg/autostart/dev.skynit.localdesk-daemon.desktop'
echo "Arch package verified: $package"
