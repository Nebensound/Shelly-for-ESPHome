#!/bin/sh
# Builds the migration package of every model locally with mos (needs Docker)
# and writes them as mg2esphome-<Model>.zip into the given directory.
#
#   scripts/build-packages.sh packages
#   SHELLY2ESPHOME_PACKAGES_DIR=$PWD/packages cargo install --path crates/shelly2esphome
set -eu

out=${1:?usage: build-packages.sh <output directory>}
repo=$(cd "$(dirname "$0")/.." && pwd)
command -v mos >/dev/null || { echo "mos not found: https://mongoose-os.com/docs/mongoose-os/quickstart/setup.md" >&2; exit 1; }
mkdir -p "$out"
out=$(cd "$out" && pwd)

# The Mongoose OS libraries are submodules pinned to exact commits; the
# libraries without sources are fetched as binaries of exactly those commits.
git -C "$repo" submodule update --init --depth 1 migration_firmware/deps
sh "$repo/scripts/fetch-prebuilt.sh" "$repo/migration_firmware/build/prebuilt"

# The supported models are the build names in the CLI's model table.
models=$(grep -o '(Model::[A-Za-z0-9]*, "[^"]*", "[^"]*"' "$repo/crates/shelly2esphome/src/model.rs" |
  sed 's/.*"\([^"]*\)"$/\1/')
for model in $models; do
  echo "building $model" >&2
  (cd "$repo/migration_firmware" && mos build --local --no-libs-update \
    --binary-libs-dir build/prebuilt --platform esp8266 --build-var "MODEL=$model")
  cp "$repo/migration_firmware/build/fw.zip" "$out/mg2esphome-$model.zip"
done
echo "$(echo "$models" | wc -w | tr -d ' ') packages in $out" >&2
