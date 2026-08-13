#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 [--build] DESTDIR" >&2
  exit 2
}

build=0
if [[ ${1:-} == "--build" ]]; then
  build=1
  shift
fi
[[ $# -eq 1 ]] || usage

workspace=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
destination=$1
[[ $destination == /* ]] || { echo "DESTDIR must be absolute" >&2; exit 2; }
mkdir -p -- "$destination"
destination=$(cd -- "$destination" && pwd -P)
[[ $destination != / ]] || { echo "refusing to stage into /" >&2; exit 2; }

if (( build )); then
  pnpm --dir "$workspace/apps/desktop-ui" build
  cargo build --manifest-path "$workspace/Cargo.toml" --release --locked --bins \
    -p localdesk-appd \
    -p localdesk-telemetry-helper \
    -p localdesk-network-helper \
    -p localdesk-remote-ssh \
    -p localdesk-desktop
fi

if [[ -n ${CARGO_TARGET_DIR:-} ]]; then
  if [[ $CARGO_TARGET_DIR == /* ]]; then
    binary_dir="$CARGO_TARGET_DIR/release"
  else
    binary_dir="$workspace/$CARGO_TARGET_DIR/release"
  fi
else
  binary_dir="$workspace/target/release"
fi
install_dir="$destination/usr/lib/localdesk"
doc_dir="$destination/usr/share/doc/localdesk"
install -d -m 0755 "$install_dir" "$doc_dir"
for name in \
  localdesk-launcher \
  localdesk-desktop \
  localdesk-appd \
  localdesk-telemetry-helper \
  localdesk-network-helper \
  localdesk-ssh-askpass
do
  install -m 0755 "$binary_dir/$name" "$install_dir/$name"
done

install -D -m 0644 \
  "$workspace/packaging/linux/dev.skynit.localdesk.desktop" \
  "$destination/usr/share/applications/dev.skynit.localdesk.desktop"
install -D -m 0644 \
  "$workspace/packaging/linux/dev.skynit.localdesk-daemon.desktop" \
  "$destination/etc/xdg/autostart/dev.skynit.localdesk-daemon.desktop"
install -D -m 0644 \
  "$workspace/apps/desktop-ui/src-tauri/icons/icon.png" \
  "$destination/usr/share/icons/hicolor/1024x1024/apps/dev.skynit.localdesk.png"
install -m 0644 "$workspace/crates/network/README.md" "$doc_dir/network-collector.md"
install -m 0644 "$workspace/packaging/linux/README.md" "$doc_dir/packaging.md"
install -D -m 0644 "$workspace/LICENSE" "$destination/usr/share/licenses/localdesk/LICENSE"

(
  cd "$destination"
  sha256sum \
    usr/lib/localdesk/localdesk-launcher \
    usr/lib/localdesk/localdesk-desktop \
    usr/lib/localdesk/localdesk-appd \
    usr/lib/localdesk/localdesk-telemetry-helper \
    usr/lib/localdesk/localdesk-network-helper \
    usr/lib/localdesk/localdesk-ssh-askpass \
    > usr/share/doc/localdesk/artifact-manifest.sha256
)

echo "staged LocalDesk release at $destination"
