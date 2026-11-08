# Shelly for ESPHome

Flash **your own ESPHome firmware** onto Shelly Gen1 devices over the air —
no cable, no Tasmota in between.

The `shelly2esphome` command line tool talks to a device running the Shelly
stock firmware, installs a small intermediate firmware (the *migration
firmware*), and lets it download, verify and install the ESPHome
`firmware.bin` you built yourself, Wi-Fi settings included.

> **Status:** prototype. The procedure has been tested against a simulated
> device; test it on a spare device before migrating devices that are hard to
> reach.

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
- Use the **uncompressed** `firmware.bin` (ESPHome dashboard: *Download →
  Legacy/factory format*), not `firmware.ota.bin.gz`.

### 2. Get the migration package for the model

CI builds one package per model: `mg2esphome-<Model>.zip`, for example
`mg2esphome-Shelly1PM.zip`. Download it from the artifacts of the CI run
(Actions tab). `shelly2esphome probe` tells you which file the device needs.

### 3. Run the tool

Install the tool with `cargo install --git https://github.com/Nebensound/Shelly-for-ESPHome shelly2esphome`.
Alternatively, download the macOS binary from the CI artifacts
(`shelly2esphome-macos-universal`). Then run:

```sh
shelly2esphome probe 10.10.5.79
shelly2esphome check firmware.bin
shelly2esphome flash 10.10.5.79 --firmware firmware.bin --migration mg2esphome-Shelly1PM.zip
```

`flash` shows a summary and asks for confirmation before it changes anything.
It refuses to run when:

- the device type is unsupported,
- the package was built for another model,
- the device's Wi-Fi signal is weaker than `--min-rssi` (default −75 dBm;
  `--allow-weak-signal` overrides this deliberately),
- the stock firmware requires a login that was not given (`--auth user:password`).

The device downloads both files from your computer. It must be able to reach
your computer on the chosen port; set `--serve-ip`/`--port` if the address
is not detected correctly or a firewall sits in between (e.g. between VLANs).

## Recovery

- **Device waits in the migration firmware:** run `flash` again; it resumes
  without the package. `probe` shows the state and the last error.
- **Migration firmware cannot join your Wi-Fi:** it falls back to the SSID
  `mgos-recover` with the password `RJoPuKC3u5`. Start a hotspot with these
  credentials and run the tool against the address the device gets there.
- **Back to the Shelly firmware before installing ESPHome:** while the
  migration firmware waits, `POST` the stock firmware ZIP from
  `https://api.shelly.cloud/files/firmware` to `http://<device>/update`.
- **After ESPHome is installed:** going back to the Shelly firmware needs a
  serial connection.

## Security

While the migration firmware waits for `MG2X.Flash`, anyone in the same
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

The migration firmware itself is built by CI with
[`mos`](https://mongoose-os.com/docs/mongoose-os/quickstart/setup.md). To build
it locally, run
`mos build --local --platform esp8266 --build-var MODEL=Shelly1PM` in
`migration_firmware/`.

See [AGENTS.md](AGENTS.md) for architecture, invariants and conventions.

## License

- `migration_firmware/`: GPL-3.0 (inherited from mgos-to-tasmota), see
  [migration_firmware/LICENSE](migration_firmware/LICENSE).
- The CLI's license is not decided yet.
