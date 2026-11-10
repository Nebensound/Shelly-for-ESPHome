#!/bin/sh
# Downloads the prebuilt Mongoose OS libraries listed in
# migration_firmware/prebuilt.sha256 into the given directory and checks their
# SHA-256 digests. Pass that directory to `mos build --binary-libs-dir`.
#
#   scripts/fetch-prebuilt.sh migration_firmware/build/prebuilt
set -eu

out=${1:?usage: fetch-prebuilt.sh <output directory>}
repo=$(cd "$(dirname "$0")/.." && pwd)
lock="$repo/migration_firmware/prebuilt.sha256"
mkdir -p "$out"

if command -v sha256sum >/dev/null; then
  check() { sha256sum -c --quiet -; }
else
  check() { shasum -a 256 -c --quiet -; }
fi

grep -v '^#' "$lock" | while read -r digest file; do
  [ -n "$file" ] || continue
  # libota-common-esp8266-latest.a -> lib "ota-common", asset libota-common-esp8266.a
  name=${file#lib}
  name=${name%-esp8266-latest.a}
  url="https://github.com/mongoose-os-libs/$name/releases/download/latest/lib$name-esp8266.a"
  if ! { [ -f "$out/$file" ] && (cd "$out" && echo "$digest  $file" | check) >/dev/null 2>&1; }; then
    curl -fsSL --retry 3 -o "$out/$file" "$url"
    (cd "$out" && echo "$digest  $file" | check)
  fi
  echo "$file ok" >&2
done
