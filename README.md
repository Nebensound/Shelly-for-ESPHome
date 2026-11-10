# Shelly for ESPHome

Flash **your own ESPHome firmware** onto Shelly Gen1 devices over the air —
no cable, no Tasmota in between.

The `shelly2esphome` command line tool talks to a device running the Shelly
stock firmware, installs a small intermediate firmware (the *migration
firmware*), and lets it download, verify and install the ESPHome
`firmware.bin` you built yourself, Wi-Fi settings included.

> **Status:** prototype. Tested end to end against a simulated device and on
> real hardware (November 2026, stock firmware 1.14.0):
>
> | Device | Signal | Result |
> |---|---|---|
> | Shelly 1PM (`SHSW-PM`) | −81 dBm | ESPHome running |
> | Shelly Dimmer 2 (`SHDM-2`), in the wall | −83 dBm | ESPHome running, STM32 reflashed by ESPHome |
>
> Other models are built by CI but not yet tested on hardware.

## How it works

```text
 your computer (shelly2esphome)                 Shelly Gen1
 ───────────────────────────────                ───────────────────────────────
 1. checks image, package, model, Wi-Fi  ──►    stock firmware (/shelly, /status)
 2. serves mg2esphome-<Model>.zip        ──►    stock OTA: /ota?url=…  (verified
                                                by the stock firmware itself)
                                                restarts into migration firmware
 3. MG2X.Flash {url, size, sha256}       ──►    downloads firmware.bin into the
    serves firmware.bin                         staging slot, checks size, magic
                                                byte and SHA-256
 4. follows MG2X.Status                  ◄──    only after a successful check:
                                                moves the image to 0x0, writes
                                                bootloader last, restarts
 5. waits for the ESPHome API (6053)     ◄──    your ESPHome firmware
```

Until step 4 starts, every failure leaves the device reachable over Wi-Fi:

- If the stock firmware rejects the package or never downloads it, nothing
  changes.
- If the download or the check of `firmware.bin` fails, the device stays in the
  migration firmware and accepts a new attempt: just run `flash` again.

The migration firmware is based on
[mgos-to-tasmota](https://github.com/yaourdt/mgos-to-tasmota) (GPL-3.0).

## Supported devices

Shelly 1, 1PM, 1L, 2, 2.5, Plug S, Plug US, RGBW2 (colour mode), Dimmer 1,
Dimmer 2, EM, Bulb, Vintage, Duo, Duo RGBW, i3 and UNI.

Battery-powered devices (H&T) and the Plug 2 (4 MB flash) are not supported.

## Usage

### 1. Build your ESPHome firmware

- The target is ESP8266, for Gen1 Shellys usually `board: esp01_1m`.
- Set your real Wi-Fi credentials. Also configure a fallback access point
  (`wifi: ap:`) and `captive_portal:`, so the device stays reachable if the
  credentials are wrong.
- Enable `api:` and `ota:`. The tool detects success on the API port 6053, and
  later updates then run through ESPHome.
- Use the **uncompressed** single-image firmware (ESPHome dashboard:
  *Download → Firmware-Image*), not the ELF file and not `firmware.ota.bin.gz`.
- Shelly Dimmer 1/2: use the `shelly_dimmer` light with `firmware: update:
  true`. ESPHome then reflashes the STM32 co-processor on first boot; there is
  no way back to the Shelly STM32 firmware.

### 2. Get the tool

The tool carries the migration packages (`mg2esphome-<Model>.zip`) of all
supported models; `flash` detects the model and picks the matching one.

- **From crates.io** (needs a [Rust toolchain](https://rustup.rs)):
  `cargo install shelly2esphome`. The crate carries the packages of its own
  version, so the build needs no further download.
- **From the repository:**
  `cargo install --git https://github.com/Nebensound/Shelly-for-ESPHome shelly2esphome`.
  The build downloads the packages of the release that matches the crate
  version (needs `curl`).
- **Build the packages yourself** (needs
  [`mos`](https://mongoose-os.com/docs/mongoose-os/quickstart/setup.md) and
  Docker): `SHELLY2ESPHOME_BUILD_PACKAGES=1 cargo install --git …`, or
  `scripts/build-packages.sh packages` followed by
  `SHELLY2ESPHOME_PACKAGES_DIR=$PWD/packages cargo install --path crates/shelly2esphome`.

Packages that do not speak the tool's protocol are not embedded. A build
without packages still works; then pass the package with `--migration`.
`shelly2esphome probe` tells you whether the package is built in.

### 3. Run the tool

```sh
shelly2esphome list 10.10.5.0/24          # find Shellys and see which can be flashed
shelly2esphome probe 10.10.5.79           # details of one device
shelly2esphome backup 10.10.5.79          # save the stock settings (flash does this too)
shelly2esphome check firmware.bin         # check your firmware without a device
shelly2esphome flash 10.10.5.79 --firmware firmware.bin --port 8266
```

`list` (alias `detect`) asks every address of the given networks for
`/shelly`; without an argument it scans the /24 of your computer. Devices in
other VLANs need their network as argument. It only sends reading requests.

`backup` saves the stock settings as JSON files (`/shelly`, `/status`,
`/settings`, `/settings/actions` and the settings of every relay, light and
roller) into a new directory `shelly-backup-<type>-<mac>-<UTC time>`. They are
lost after the migration, so use them to reproduce your setup in ESPHome. The
files can contain the device's own access point password; on macOS and Linux
only your user can read them. `flash` makes the
same backup after the confirmation and before the first change
(`--backup-dir`, `--no-backup`); if it fails, nothing is changed.

`flash` shows a summary and asks for confirmation before it changes anything.
It refuses to run when:

- `firmware.bin` is incomplete or damaged (segments, checksums and the
  embedded CRC are checked) or too large for the device,
- the package is not this project's migration firmware in the matching
  protocol version,

- the device type is unsupported,
- the package was built for another model,
- the device's Wi-Fi signal is weaker than `--min-rssi` (default −75 dBm;
  `--allow-weak-signal` overrides this deliberately),
- the stock firmware requires a login that was not given (`--auth user:password`).

The device downloads both files from your computer, so **the device must be
able to open a TCP connection to your computer**. In segmented networks where
only the computer may connect into the IoT network, add a temporary firewall
rule for the migration:

| Action | Source | Destination | Protocol / port |
|---|---|---|---|
| allow | the Shelly's IP | your computer's IP | TCP, the port given with `--port` (e.g. 8266) |

Everything else (control via port 80, ESPHome detection via port 6053) is
initiated by your computer. Remove the rule afterwards. If the device cannot
reach your computer, `flash` stops with "the device did not fetch the migration package" and
nothing is changed. Set `--serve-ip` if the detected address of your computer
is wrong.

## Recovery

- **Device waits in the migration firmware:** run `flash` again; it resumes
  without the package. `probe` shows the state and the last error.
- **Migration firmware could not move itself into slot 1** (`probe` shows
  `RelocationFailed`): it restarts and retries on its own; run `flash` again a
  minute later.
- **Installing the new boot sectors failed:** the migration firmware writes the
  original boot sectors back and reports `install_retryable`; run `flash` again.
  On `install_fatal` (writing and restoring both failed) the boot sectors are
  broken, but the migration firmware keeps running and accepts another attempt:
  do not cut power or restart the device, run `flash` again right away. Only if
  that fails too, or the device loses power, does it need serial recovery.
- **Migration firmware cannot join your Wi-Fi:** it falls back to the SSID
  `mgos-recover` with the password `RJoPuKC3u5`. Start a hotspot with these
  credentials and run the tool against the address the device gets there.
- **Back to the Shelly firmware before installing ESPHome:** while the
  migration firmware waits, `POST` the stock firmware ZIP from
  `https://api.shelly.cloud/files/firmware` to `http://<device>/update`.
- **After ESPHome is installed:** going back to the Shelly firmware needs a
  serial connection.

## Security

- The tool serves `firmware.bin` (which contains your Wi-Fi credentials) under
  a random path per run and only to the device's IP address.
- The migration firmware serves no files over HTTP; only `/rpc` and the
  `/update` recovery endpoint exist.
- While the migration firmware waits for `MG2X.Flash`, anyone in the same
  network can hand it a firmware image. This is the same exposure as a stock
  Shelly without login. Run the migration in one go and do not leave devices
  waiting.

## Development

```sh
# CLI (Rust, workspace in the repository root)
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features

# Migration firmware: host tests of the hardware-independent modules
make -C migration_firmware/test check      # ASan/UBSan
make -C migration_firmware/test coverage   # 100% gate, needs gcovr
```

The migration firmware is built with
[`mos`](https://mongoose-os.com/docs/mongoose-os/quickstart/setup.md) and
Docker. Every library, module and the toolchain come from the fixed Mongoose
OS release 2.20.0 (`libs_version` in `migration_firmware/mos.yml`), so a build
does not change when upstream does. CI uses `mos` 2.20.0 as well.

To build it locally (`GITHUB_TOKEN` is optional; GitHub may refuse anonymous
clones under load):

```sh
cd migration_firmware
mos build --local --platform esp8266 --build-var MODEL=Shelly1PM
# or all models: GITHUB_TOKEN=$(gh auth token) scripts/build-packages.sh packages
```

Where the CLI build takes the migration packages from is described in
`crates/shelly2esphome/build.rs`: `SHELLY2ESPHOME_PACKAGES_DIR`,
`SHELLY2ESPHOME_BUILD_PACKAGES=1`, `crates/shelly2esphome/packages/` (filled
by CI for the published crate), or the release download (switched off with
`SHELLY2ESPHOME_NO_DOWNLOAD=1`, as in CI).

Releases are automatic. CI derives the crate version from the Conventional
Commits since the last release tag (`feat` raises the minor version, `!` or
`BREAKING CHANGE` the major version, everything else the patch version; for
squash merges the pull request title counts too) and commits it to every pull
request branch of this repository. Pull requests from forks must set it
themselves; CI names the expected version. A version raised by hand is kept if
it is higher. The first release takes the version from `Cargo.toml`. Every push to `main` that passes CI is released as
`v<crate version>`: a GitHub release with `mg2esphome-packages.zip` and the
single packages, and the crate on crates.io with the packages inside (needs
the repository secret `CARGO_REGISTRY_TOKEN`). A version on `main` that is not
released yet counts as taken, and a changed pull request title runs CI again.
Still bring a branch up to date with `main` before merging: a version computed
before another merge can collide with it, and the run on `main` then fails
instead of releasing. As long as branches are merged up to date, every merge
gets a new version, so a protocol change always ships with its own packages.

See [AGENTS.md](https://github.com/Nebensound/Shelly-for-ESPHome/blob/main/AGENTS.md) for architecture, invariants and conventions.

## License

- The CLI (`crates/shelly2esphome/`): MIT, see
  [crates/shelly2esphome/LICENSE](https://github.com/Nebensound/Shelly-for-ESPHome/blob/main/crates/shelly2esphome/LICENSE).
- `migration_firmware/`: GPL-3.0 (inherited from mgos-to-tasmota), see
  [migration_firmware/LICENSE](https://github.com/Nebensound/Shelly-for-ESPHome/blob/main/migration_firmware/LICENSE).

The CLI binary carries the migration firmware packages as data; the firmware
stays a separate GPL-3.0 program. The published crate is therefore licensed
`MIT AND GPL-3.0-only` and includes both licenses and
a [NOTICE](https://github.com/Nebensound/Shelly-for-ESPHome/blob/main/crates/shelly2esphome/NOTICE) that points to the firmware's source.
