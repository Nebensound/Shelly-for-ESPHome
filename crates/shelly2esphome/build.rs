//! Embeds the migration packages into release builds.
//!
//! If `SHELLY2ESPHOME_PACKAGES_DIR` points to a directory, every
//! `mg2esphome-<Model>.zip` in it is compiled into the binary, so `flash`
//! needs no `--migration` option. Without the variable the binary contains no
//! packages (e.g. `cargo install`), and `--migration` is required.

use std::{env, fmt::Write as _, fs, path::PathBuf};

const PACKAGES_DIR: &str = "SHELLY2ESPHOME_PACKAGES_DIR";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed={PACKAGES_DIR}");
    let mut entries = Vec::new();
    if let Some(dir) = env::var_os(PACKAGES_DIR).filter(|dir| !dir.is_empty()) {
        let dir = fs::canonicalize(PathBuf::from(dir))?;
        println!("cargo:rerun-if-changed={}", dir.display());
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("mg2esphome-") && name.ends_with(".zip") {
                println!("cargo:rerun-if-changed={}", path.display());
                entries.push((name.to_owned(), path));
            }
        }
        if entries.is_empty() {
            return Err(format!("{PACKAGES_DIR}={} enthält keine Pakete", dir.display()).into());
        }
    }
    entries.sort();

    let mut code = String::from("/// Migration packages compiled into this binary.\n");
    code.push_str("pub static PACKAGES: &[EmbeddedPackage] = &[\n");
    for (name, path) in &entries {
        writeln!(code, "    ({name:?}, include_bytes!({:?})),", path.display().to_string())?;
    }
    code.push_str("];\n");
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR fehlt")?);
    fs::write(out.join("embedded_packages.rs"), code)?;
    Ok(())
}
