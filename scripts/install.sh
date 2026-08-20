#!/bin/sh

set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
plugin_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
manifest="$plugin_root/herdr-plugin.toml"

version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$manifest" | sed -n '1p')
if [ -z "$version" ]; then
  echo "herdr-tilt: could not read the plugin version from $manifest" >&2
  exit 1
fi

os=${HERDR_TILT_INSTALL_OS:-$(uname -s)}
arch=${HERDR_TILT_INSTALL_ARCH:-$(uname -m)}

case "$os:$arch" in
  Darwin:arm64|Darwin:aarch64) platform=macos-aarch64 ;;
  Darwin:x86_64|Darwin:amd64) platform=macos-x86_64 ;;
  Linux:arm64|Linux:aarch64) platform=linux-aarch64 ;;
  Linux:x86_64|Linux:amd64) platform=linux-x86_64 ;;
  *)
    echo "herdr-tilt: no prebuilt binary is available for $os/$arch" >&2
    exit 1
    ;;
esac

asset="herdr-tilt-$platform"
base_url=${HERDR_TILT_RELEASE_BASE_URL:-"https://github.com/the-inconvenience-store/herdr-tilt/releases/download/v$version"}
install_dir="$plugin_root/target/release"
mkdir -p "$install_dir"

tmp_dir=$(mktemp -d "$install_dir/.herdr-tilt-install.XXXXXX")
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

download() {
  url=$1
  destination=$2
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error --output "$destination" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget --quiet --output-document="$destination" "$url"
  else
    echo "herdr-tilt: installing requires curl or wget" >&2
    exit 1
  fi
}

download "$base_url/$asset" "$tmp_dir/$asset"
download "$base_url/$asset.sha256" "$tmp_dir/$asset.sha256"

(
  cd "$tmp_dir"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum --check "$asset.sha256"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 --check "$asset.sha256"
  else
    echo "herdr-tilt: checksum verification requires sha256sum or shasum" >&2
    exit 1
  fi
)

chmod 755 "$tmp_dir/$asset"
mv -f "$tmp_dir/$asset" "$install_dir/herdr-tilt"

echo "Installed herdr-tilt $version for $platform"
