#!/bin/sh
# Builds the migration package of every model locally with mos (needs Docker)
# and writes them as mg2esphome-<Model>.zip into the given directory.
#
#   GITHUB_TOKEN=$(gh auth token) scripts/build-packages.sh packages   # token optional
#   SHELLY2ESPHOME_PACKAGES_DIR=$PWD/packages cargo install --path crates/shelly2esphome
set -eu

out=${1:?usage: build-packages.sh <output directory>}
repo=$(cd "$(dirname "$0")/.." && pwd)
command -v mos >/dev/null || { echo "mos not found: https://mongoose-os.com/docs/mongoose-os/quickstart/setup.md" >&2; exit 1; }
mkdir -p "$out"
out=$(cd "$out" && pwd)

# mos.yml pins every library to the Mongoose OS release 2.20.0.
# GitHub may refuse anonymous clones under load; with GITHUB_TOKEN set, mos
# clones authenticated (the token is passed as a file, not on the command line).
credentials=""
if [ -n "${GITHUB_TOKEN:-}" ]; then
  creds_file=$(mktemp)
  trap 'rm -f "$creds_file"' EXIT
  printf 'github.com:%s\n' "$GITHUB_TOKEN" > "$creds_file"
  credentials="--credentials @$creds_file"
fi

# The supported models are the build names in the CLI's model table.
models=$(grep -o '(Model::[A-Za-z0-9]*, "[^"]*", "[^"]*"' "$repo/crates/shelly2esphome/src/model.rs" |
  sed 's/.*"\([^"]*\)"$/\1/')
for model in $models; do
  echo "building $model" >&2
  # shellcheck disable=SC2086 # $credentials is empty or two words on purpose
  (cd "$repo/migration_firmware" && mos build --local --platform esp8266 --build-var "MODEL=$model" $credentials)
  cp "$repo/migration_firmware/build/fw.zip" "$out/mg2esphome-$model.zip"
done
echo "$(echo "$models" | wc -w | tr -d ' ') packages in $out" >&2
