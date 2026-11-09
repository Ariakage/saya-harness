#!/bin/sh
# CI-only native backend prerequisites. Installs a verified non-setuid Bubblewrap build.
set -eu
[ "$(uname -s)" = Linux ]
version=0.13.1
checksum=0e85c8702248b8dd64144f594147553650cdd0e4c951690345faace79c217c4d
build_root=$(mktemp -d "${RUNNER_TEMP:-/tmp}/saya-bwrap.XXXXXX")
trap 'rm -rf "$build_root"' EXIT HUP INT TERM
sudo apt-get update
sudo apt-get install --no-install-recommends -y meson ninja-build pkg-config libcap-dev socat
archive="$build_root/bubblewrap.tar.xz"
if ! curl --fail --location --retry 3 --connect-timeout 15 --max-time 120 \
  --output "$archive" \
  "https://github.com/containers/bubblewrap/releases/download/v$version/bubblewrap-$version.tar.xz"; then
  # This official asset contains the same archive and uses the digest below.
  curl --fail --location --retry 3 --connect-timeout 15 --max-time 120 \
    --header 'Accept: application/octet-stream' \
    --header 'X-GitHub-Api-Version: 2022-11-28' --output "$archive" \
    'https://api.github.com/repos/containers/bubblewrap/releases/assets/640310180'
fi
printf '%s  %s\n' "$checksum" "$archive" | sha256sum --check --strict -
tar -xf "$archive" -C "$build_root"
meson setup "$build_root/build" "$build_root/bubblewrap-$version" \
  --prefix=/usr/local --buildtype=release -Dman=disabled -Dtests=false \
  -Dbash_completion=disabled -Dzsh_completion=disabled -Dselinux=disabled
meson compile -C "$build_root/build"
sudo meson install -C "$build_root/build"
test ! -u /usr/local/bin/bwrap
/usr/local/bin/bwrap --version
socat -V 2>&1 | head -n 2
