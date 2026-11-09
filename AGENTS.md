# Contribution Rules for This Repository

The [Nebensound Coding Guidelines](https://github.com/Nebensound/Nebensound-Coding-Guidlines)
apply. This file adds what is specific to this repository.

## Purpose

Flash a **user-built** ESPHome firmware onto Shelly Gen1 devices over the air.
The universal, CI-built ESPHome firmwares and the web installer that used to
live here were removed on purpose; do not reintroduce them.

## Architecture

- `crates/shelly2esphome/`: Rust CLI (binary plus library for tests).
  - `model.rs` maps device types to migration build names and stock app names.
  - `image.rs` validates ESP8266 images.
  - `migration.rs` validates the migration package.
  - `device.rs` holds the HTTP client for the stock and migration firmware.
  - `server.rs` is the local file server.
  - `flash.rs` holds the procedure and its safety checks.
  - `main.rs` is the console UI.
- `migration_firmware/`: Mongoose OS intermediate firmware (C, GPL-3.0).
  - `src/main.c` is the hardware glue: rboot, SPI flash, HTTP, RPC.
  - `src/mg2x_job.c`, `src/mg2x_writer.c`, `src/mg2x_install.c` and
    `src/mg2x_sha256.c` are hardware independent and unit tested on the host
    (`test/`, 100 % line/branch/function coverage). The installer tests inject
    a failure at every single flash operation.

## Invariants

1. **Nothing outside the staging slot is written before the image is fully
   verified.** That covers the received size equal to the announced size, the
   first byte `0xE9`, and the SHA-256 digest. The boot config and the
   bootloader are written as the very last steps; the original ones are kept
   in scratch sectors and written back if that fails.
2. **A failed job never leaves the device unreachable.** The migration
   firmware keeps running and accepts a new `MG2X.Flash`.
3. The CLI performs every refusal (model, package, signal, login, confirmation)
   **before** the first request that changes the device.
4. `MG2X_PROTOCOL_VERSION` and the marker `shelly2esphome-mg2x-protocol=N` in
   `main.c` and `MIGRATION_PROTOCOL` in `device.rs` change together. The RPC
   field names (`url`, `size`, `sha256`, `state`, `received`, `max_size`,
   `error`) and state names are the contract between both.
5. The model table in `model.rs` mirrors the `conds` in
   `migration_firmware/mos.yml`. CI checks every built package's manifest name
   against `mos.yml`.
6. The migration firmware never serves files over HTTP
   (`http.document_root` is empty); its filesystem holds the Wi-Fi
   credentials.

## Conventions

- Code, comments, commits and documentation are in English. User-facing CLI
  output is German.
- Conventional Commits. Branches are named `feat/…`, `fix/…` or `chore/…`.
- Run the checks listed in the README before committing. CI runs the same
  checks.

## Known deviations

- The central Nebensound workflow (`.nebensound/repo.toml`) is not used yet
  because `github-automation` is not ready for this project type. `ci.yml` is
  the transitional local gate. Migration target: switch once the central Rust
  workflow is usable.
- The migration firmware can only be built with `mos` (Docker). It is built in
  CI, not in every local environment.
- `main.c` (hardware glue) cannot run on the host. Its behaviour is covered by
  the CLI's end-to-end tests against a simulated device and by tests on real
  hardware.
- The CLI coverage gate (100%) is enforced in CI. The console UI in `main.rs`
  is not yet covered by tests.
