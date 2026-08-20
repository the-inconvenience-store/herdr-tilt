#!/bin/sh

set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 v<version>" >&2
  exit 2
fi

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH='' cd -- "$script_dir/.." && pwd)
tag=$1
expected=${tag#v}

if [ "$tag" = "$expected" ] || [ -z "$expected" ]; then
  echo "release tag must start with v (for example, v0.1.0)" >&2
  exit 1
fi

cargo_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$repo_root/Cargo.toml" | sed -n '1p')
plugin_version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$repo_root/herdr-plugin.toml" | sed -n '1p')

if [ "$cargo_version" != "$expected" ]; then
  echo "Cargo.toml version $cargo_version does not match release tag $tag" >&2
  exit 1
fi

if [ "$plugin_version" != "$expected" ]; then
  echo "herdr-plugin.toml version $plugin_version does not match release tag $tag" >&2
  exit 1
fi

echo "Release versions match $tag"
