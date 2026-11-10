# Contribution Rules for This Repository

These rules apply to every change, by people and by coding agents.

## Engineering rules

- **Types carry meaning.** Model domain values (models, versions, sizes,
  digests, addresses) as types, not loose strings or integers. No `unsafe`
  Rust; `unwrap` is denied outside tests.
- **Errors are handled, not hidden.** Every failure reaches the user with
  its context; production code does not panic. An error path that cannot
  occur is written without a branch rather than left untested (see Known
  deviations).
- **Formatter and linter are clean.** `cargo fmt`, `cargo clippy -D warnings`
  and `clang-format` for the firmware, with the same settings locally and in
  CI.
- **100 % coverage of production code.** Rust: regions, lines and functions.
  The host-tested firmware modules: lines, branches and functions. Every
  exclusion needs a specific reason next to it.
- **Tests come with the change.** New behaviour and every fixed bug get a
  test that fails without the change.
- **Security.** No secrets in code, tests, fixtures or logs; workflows get
  only the permissions they need. Respect the invariants below.
- **Documentation.** Public items are documented (`missing_docs` is denied).
  Comments explain why, invariants and pitfalls, not what the code says.
- **Focused changes.** One concern per pull request, with a description of
  what changes and why, and the checks listed in the README passing
  locally first.

## Purpose

Flash a **user-built** ESPHome firmware onto Shelly Gen1 devices over the air.
The universal, CI-built ESPHome firmwares and the web installer that used to
live here were removed on purpose; do not reintroduce them.

## Architecture

- `crates/shelly2esphome/`: Rust CLI (binary plus library for tests).
  - `model.rs` maps device types to migration build names and stock app names.
  - `image.rs` validates ESP8266 images.
  - `migration.rs` validates the migration package.
  - `embedded.rs` holds the migration packages that `build.rs` compiles in:
    from `SHELLY2ESPHOME_PACKAGES_DIR`, from `scripts/build-packages.sh`
    (`SHELLY2ESPHOME_BUILD_PACKAGES=1`), from the crate's `packages/` (filled
    by CI for the crates.io package) or downloaded from the release
    `v<crate version>`. Only packages of the current protocol are embedded.
    An explicit `--migration` always takes precedence; embedded packages pass
    the same validation as files.
  - `protocol.rs` holds the protocol version and marker search; `build.rs`
    includes it, so it must not import anything from the crate.
  - `backup.rs` saves the stock settings; `discover.rs` scans networks for
    `list`. Both only send reading requests.
  - `device.rs` holds the HTTP client for the stock and migration firmware.
  - `server.rs` is the local file server.
  - `flash.rs` holds the procedure and its safety checks.
  - `cli.rs` is the console UI (arguments, output, prompts); `main.rs` only
    connects it to the process. Tests drive `cli::run` with their own
    writers and settings.
  - Unit tests live in `src/unit_tests/<module>.rs` (included with
    `#[path]`), integration tests in `tests/`; the simulated devices are in
    `tests/common/`. Neither counts as production code for coverage.
- `migration_firmware/`: Mongoose OS intermediate firmware (C, GPL-3.0).
  The CLI is MIT; it carries the firmware packages only as data. The published
  crate is `MIT AND GPL-3.0-only` and ships both licenses plus
  `crates/shelly2esphome/NOTICE`; the release bundle carries the GPL-3.0
  license of the packages.
  `crates/shelly2esphome/LICENSE-migration-firmware` is a copy of
  `migration_firmware/LICENSE`, because a crate can only contain files below
  its own directory.
  - `src/main.c` is the hardware glue: rboot, SPI flash, HTTP, RPC.
  - `src/mg2x_job.c`, `src/mg2x_writer.c`, `src/mg2x_install.c` and
    `src/mg2x_sha256.c` are hardware independent and unit tested on the host
    (`test/`, 100 % line/branch/function coverage). The installer tests inject
    a failure at every single flash operation.
  - `mos.yml` pins every library, module and the toolchain to the Mongoose
    OS release 2.20.0 (`libs_version`); CI installs `mos` 2.20.0 and builds
    with `mos build --local` and authenticated clones.

## Invariants

1. **Nothing outside the staging slot is written before the image is fully
   verified.** That covers the received size equal to the announced size, the
   first byte `0xE9`, and the SHA-256 digest. The boot config and the
   bootloader are written as the very last steps; the original ones are kept
   in scratch sectors and written back if that fails.
2. **A failed job never leaves the device unreachable.** The migration
   firmware keeps running and accepts a new `MG2X.Flash`.
3. The CLI performs every refusal (model, package, signal, login, confirmation)
   and the backup of the stock settings **before** the first request that
   changes the device.
4. `MG2X_PROTOCOL_VERSION` and the marker `shelly2esphome-mg2x-protocol=N` in
   `main.c` and `MIGRATION_PROTOCOL` in `protocol.rs` change together, and
   with them the crate version (builds download the release packages of
   their own version). The RPC
   field names (`url`, `size`, `sha256`, `state`, `received`, `max_size`,
   `error`) and state names are the contract between both.
5. The model table in `model.rs` mirrors the `conds` in
   `migration_firmware/mos.yml`. CI checks every built package's manifest name
   against `mos.yml`.
6. The migration firmware never serves files over HTTP
   (`http.document_root` is empty); its filesystem holds the Wi-Fi
   credentials.

## Conventions

- Code, comments, commits, documentation and all CLI output are in English.
- Conventional Commits. Branches are named `feat/…`, `fix/…` or `chore/…`.
- Run the checks listed in the README before committing. CI runs the same
  checks.

## Known deviations

- The migration firmware can only be built with `mos` (Docker). It is built in
  CI, not in every local environment.
- `main.c` (hardware glue) cannot run on the host. Its behaviour is covered by
  the CLI's end-to-end tests against a simulated device and by tests on real
  hardware.
- Error paths that cannot occur in practice are written without a branch
  instead of being left untested (for example `unwrap_or_default` for building
  an HTTP client without TLS backends), so the 100% region gate stays honest.
