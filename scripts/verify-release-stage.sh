#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 1 ]] || { echo "usage: $0 DESTDIR" >&2; exit 2; }
destination=$1
[[ $destination == /* ]] || { echo "DESTDIR must be absolute" >&2; exit 2; }
destination=$(cd -- "$destination" && pwd -P)
[[ $destination != / ]] || { echo "refusing to verify /" >&2; exit 2; }

install_dir="$destination/usr/lib/localdesk"
command -v objdump >/dev/null || { echo "objdump is required for portable ISA verification" >&2; exit 1; }
for name in \
  localdesk-launcher \
  localdesk-desktop \
  localdesk-appd \
  localdesk-telemetry-helper \
  localdesk-network-helper \
  localdesk-ssh-askpass
do
  path="$install_dir/$name"
  [[ -f $path && ! -L $path && -x $path ]] || { echo "invalid binary: $path" >&2; exit 1; }
  mode=$(stat -c '%a' "$path")
  [[ $mode == 755 ]] || { echo "unexpected mode $mode: $path" >&2; exit 1; }
  if getcap "$path" | grep -q .; then
    echo "staged binaries must not carry file capabilities: $path" >&2
    exit 1
  fi
  if ldd "$path" | grep -q 'not found'; then
    echo "unresolved runtime dependency: $path" >&2
    exit 1
  fi
  if objdump -d --section=.text "$path" | awk '
    /%zmm[0-9]+|%k[0-7]([^0-9]|$)/ { found = 1 }
    END { exit !found }
  '; then
    echo "x86_64 release binary contains AVX-512 instructions: $path" >&2
    exit 1
  fi
done

[[ -f $destination/usr/share/applications/dev.skynit.localdesk.desktop ]]
[[ -f $destination/etc/xdg/autostart/dev.skynit.localdesk-daemon.desktop ]]
[[ -f $destination/usr/share/icons/hicolor/1024x1024/apps/dev.skynit.localdesk.png ]]
[[ -f $destination/usr/share/doc/localdesk/packaging.md ]]
[[ -f $destination/usr/share/doc/localdesk/artifact-manifest.sha256 ]]
if find "$destination" -type f \( -name '*.service' -o -name '*.socket' \) -print -quit | grep -q .; then
  echo "release stage unexpectedly contains a systemd unit" >&2
  exit 1
fi

(
  cd "$destination"
  sha256sum -c usr/share/doc/localdesk/artifact-manifest.sha256
)
XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:?XDG_RUNTIME_DIR is required} \
  "$install_dir/localdesk-launcher" --check
echo "release stage verified: $destination"
