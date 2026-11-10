#!/bin/sh
# Fetches the Mongoose OS libraries that are only published as binaries for
# the commits pinned by the submodules in migration_firmware/deps, into the
# given directory. Pass that directory to `mos build --binary-libs-dir`.
#
#   scripts/fetch-prebuilt.sh migration_firmware/build/prebuilt
#
# The submodule commit is the only pin, and Dependabot updates it. A library
# is binary-only if its mos.yml declares sources but the repository has none.
# Its binary is chosen by commit, not by a checksum: the release whose tag
# points at exactly the pinned commit is used, "latest" first (built with the
# same toolchain as the rest of the pinned tree), else the newest version tag.
# If no release tag points at the pinned commit, the build stops instead of
# mixing binaries with sources of another version.
#
# Every binary is kept with a .source file (commit and tag). Binaries whose
# commit still matches are reused (CI caches this directory), so a pinned
# version keeps building after upstream has moved "latest" to a newer commit.
# manifest.txt records what was used, including the SHA-256 of each file.
set -eu

out=${1:?usage: fetch-prebuilt.sh <output directory>}
repo=$(cd "$(dirname "$0")/.." && pwd)
deps="$repo/migration_firmware/deps"
# mos looks for lib<name>-<platform>-<libs_version>.a; mos.yml sets
# libs_version to the mos version, and CI installs the "latest" mos.
platform=esp8266
libs_version=latest
mkdir -p "$out"

if command -v sha256sum >/dev/null; then
  digest() { sha256sum "$1" | cut -d' ' -f1; }
else
  digest() { shasum -a 256 "$1" | cut -d' ' -f1; }
fi

# Prints the release tag for `commit` from `git ls-remote --tags` output on
# stdin: "latest" if it points there, else the newest version tag.
release_tag() {
  # Annotated tags appear a second time as "<commit> refs/tags/<tag>^{}".
  tags=$(sed 's/[{}^]//g' | sed -n "s#^$1	refs/tags/##p" | sort -u)
  if echo "$tags" | grep -qx latest; then
    echo latest
  else
    echo "$tags" | grep -E '^[0-9]+\.[0-9]+(\.[0-9]+)?$' | sort -V | tail -n 1
  fi
}

manifest="$out/manifest.txt"
: >"$manifest.tmp"
found=0
for dir in "$deps"/*/; do
  dir=${dir%/}
  name=$(basename "$dir")
  [ -f "$dir/mos.yml" ] || continue
  grep -q '^sources:' "$dir/mos.yml" || continue
  [ -d "$dir/src" ] && continue
  found=$((found + 1))

  commit=$(git -C "$dir" rev-parse HEAD)
  file="lib$name-$platform-$libs_version.a"
  if [ -f "$out/$file" ] && [ "$(cut -d' ' -f1 "$out/$file.source" 2>/dev/null)" = "$commit" ]; then
    tag=$(cut -d' ' -f2 "$out/$file.source")
    echo "$file: reusing $tag (${commit%"${commit#???????}"})" >&2
  else
    url=$(git -C "$dir" remote get-url origin)
    tag=$(git ls-remote --tags "$url" | release_tag "$commit")
    if [ -z "$tag" ]; then
      echo "$name: no release of $url points at the pinned commit $commit." >&2
      echo "Update the submodule (the Dependabot pull request) to a released commit." >&2
      exit 1
    fi
    asset="${url%.git}/releases/download/$tag/lib$name-$platform.a"
    curl -fsSL --retry 3 -o "$out/$file.part" "$asset"
    mv "$out/$file.part" "$out/$file"
    echo "$commit $tag" >"$out/$file.source"
    echo "$file: fetched $tag (${commit%"${commit#???????}"})" >&2
  fi
  echo "$name $commit $tag $(digest "$out/$file")" >>"$manifest.tmp"
done
mv "$manifest.tmp" "$manifest"
[ "$found" -gt 0 ] || { echo "no submodules in $deps; run: git submodule update --init --depth 1 migration_firmware/deps" >&2; exit 1; }
