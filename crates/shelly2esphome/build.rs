//! Embeds the migration packages (`mg2esphome-<Model>.zip`) into the binary,
//! so `flash` finds the package of the detected model by itself.
//!
//! The packages come from the first source that works:
//!
//! 1. `SHELLY2ESPHOME_PACKAGES_DIR=<dir>`: every `mg2esphome-*.zip` in `<dir>`
//!    (CI release builds, or packages you built with
//!    `scripts/build-packages.sh`).
//! 2. `SHELLY2ESPHOME_BUILD_PACKAGES=1`: runs `scripts/build-packages.sh`
//!    (needs `mos` and Docker).
//! 3. `packages/` next to this file: the packages CI puts into the published
//!    crate, so `cargo install shelly2esphome` embeds them offline.
//! 4. The GitHub release `v<crate version>`: downloads
//!    `mg2esphome-packages.zip` with `curl` (`cargo install --git …`).
//!    Skipped with `SHELLY2ESPHOME_NO_DOWNLOAD=1` or offline builds;
//!    `SHELLY2ESPHOME_BUNDLE_URL` points to another bundle (forks, tests).
//!
//! Packages are only embedded if they speak this tool's protocol. Without
//! any packages the build still succeeds; `flash` then needs `--migration`.

#[path = "src/protocol.rs"]
#[allow(dead_code)]
mod protocol;

use std::{
    env,
    error::Error,
    fmt::Write as _,
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    process::Command,
};

const PACKAGES_DIR: &str = "SHELLY2ESPHOME_PACKAGES_DIR";
const BUILD_PACKAGES: &str = "SHELLY2ESPHOME_BUILD_PACKAGES";
const NO_DOWNLOAD: &str = "SHELLY2ESPHOME_NO_DOWNLOAD";
const BUNDLE_URL: &str = "SHELLY2ESPHOME_BUNDLE_URL";
const RELEASES: &str = "https://github.com/Nebensound/Shelly-for-ESPHome/releases/download";
const BUNDLE: &str = "mg2esphome-packages.zip";

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn is_set(name: &str) -> bool {
    env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0")
}

fn warn(message: &str) {
    println!("cargo:warning={message}");
}

/// Every `mg2esphome-*.zip` in `dir`, sorted by name.
fn packages_in(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut packages = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("mg2esphome-") && name.ends_with(".zip") {
            println!("cargo:rerun-if-changed={}", path.display());
            packages.push((name.to_owned(), path));
        }
    }
    packages.sort();
    Ok(packages)
}

/// Protocol number of a migration package, `None` if it carries no marker.
fn package_protocol(bytes: &[u8]) -> Result<Option<u32>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.name().ends_with(".bin") {
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            if let Some(found) = protocol::find_marker(&content) {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

/// Keeps only packages of this tool's protocol.
fn matching_protocol(packages: Vec<(String, PathBuf)>) -> Result<Vec<(String, PathBuf)>> {
    let mut kept = Vec::new();
    for (name, path) in packages {
        match package_protocol(&fs::read(&path)?)? {
            Some(found) if found == protocol::MIGRATION_PROTOCOL => kept.push((name, path)),
            found => warn(&format!(
                "{name}: protocol {found:?}, expected {}; not embedded",
                protocol::MIGRATION_PROTOCOL
            )),
        }
    }
    Ok(kept)
}

/// The packages shipped in the crate's `packages/` directory, `None` if there
/// are none (a source checkout).
fn bundled_packages() -> Result<Option<Vec<(String, PathBuf)>>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("no manifest dir")?);
    let dir = manifest.join("packages");
    println!("cargo:rerun-if-changed={}", dir.display());
    if !dir.is_dir() {
        return Ok(None);
    }
    let packages = packages_in(&dir)?;
    Ok((!packages.is_empty()).then_some(packages))
}

/// Runs `scripts/build-packages.sh` into `out` (needs `mos` and Docker).
fn build_packages(out: &Path) -> Result<Vec<(String, PathBuf)>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("no manifest dir")?);
    let script = manifest.join("../../scripts/build-packages.sh");
    // Rebuild the packages whenever the firmware or the model list changes.
    for input in [
        script.clone(),
        manifest.join("../../migration_firmware/mos.yml"),
        manifest.join("../../migration_firmware/src"),
        manifest.join("src/model.rs"),
    ] {
        println!("cargo:rerun-if-changed={}", input.display());
    }
    let target = out.join("built");
    let status = Command::new("sh").arg(&script).arg(&target).status()?;
    if !status.success() {
        return Err(format!("{} failed ({status})", script.display()).into());
    }
    packages_in(&target)
}

/// Downloads the package bundle of the release matching this crate version.
fn download_packages(out: &Path) -> Result<Vec<(String, PathBuf)>> {
    let version = env::var("CARGO_PKG_VERSION")?;
    // Forks and tests can point to another bundle.
    let url = env::var(BUNDLE_URL).unwrap_or_else(|_| format!("{RELEASES}/v{version}/{BUNDLE}"));
    let bundle = out.join(BUNDLE);
    let status = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--max-time", "120"])
        .arg("--output")
        .arg(&bundle)
        .arg(&url)
        .status()
        .map_err(|error| format!("curl not available: {error}"))?;
    if !status.success() {
        return Err(format!("downloading {url} failed ({status})").into());
    }
    let target = out.join("downloaded");
    fs::create_dir_all(&target)?;
    let mut archive = zip::ZipArchive::new(Cursor::new(fs::read(&bundle)?))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(name) =
            Path::new(entry.name()).file_name().and_then(|name| name.to_str()).map(str::to_owned)
        else {
            continue;
        };
        if name.starts_with("mg2esphome-") && name.ends_with(".zip") {
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            fs::write(target.join(&name), content)?;
        }
    }
    packages_in(&target)
}

fn main() -> Result<()> {
    for name in [PACKAGES_DIR, BUILD_PACKAGES, NO_DOWNLOAD, BUNDLE_URL, "CARGO_NET_OFFLINE"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);

    let packages = if let Some(dir) = env::var_os(PACKAGES_DIR).filter(|dir| !dir.is_empty()) {
        // An explicit directory must contain packages; anything else is a mistake.
        let dir = fs::canonicalize(PathBuf::from(dir))?;
        println!("cargo:rerun-if-changed={}", dir.display());
        let packages = packages_in(&dir)?;
        if packages.is_empty() {
            return Err(format!("{PACKAGES_DIR}={} contains no packages", dir.display()).into());
        }
        matching_protocol(packages)?
    } else if is_set(BUILD_PACKAGES) {
        matching_protocol(build_packages(&out)?)?
    } else if let Some(packages) = bundled_packages()? {
        matching_protocol(packages)?
    } else if is_set(NO_DOWNLOAD) || env::var("CARGO_NET_OFFLINE").is_ok_and(|v| v == "true") {
        Vec::new()
    } else {
        match download_packages(&out).and_then(matching_protocol) {
            Ok(packages) => packages,
            Err(error) => {
                warn(&format!(
                    "no migration packages embedded ({error}); flash will need --migration. \
                     See README: SHELLY2ESPHOME_PACKAGES_DIR or SHELLY2ESPHOME_BUILD_PACKAGES."
                ));
                Vec::new()
            }
        }
    };

    let mut code = String::from("/// Migration packages compiled into this binary.\n");
    code.push_str("pub static PACKAGES: &[EmbeddedPackage] = &[\n");
    for (name, path) in &packages {
        writeln!(code, "    ({name:?}, include_bytes!({:?})),", path.display().to_string())?;
    }
    code.push_str("];\n");
    fs::write(out.join("embedded_packages.rs"), code)?;
    Ok(())
}
