//! What the official package of a Gen2/Gen3 device cannot tell: whether its
//! flash layout is one that was looked at, and which bootloader `min_version`
//! makes the stock installer write ESPHome's bootloader.
//!
//! The layouts are those of the current stable packages; Shelly changes them
//! between releases (2.2.0 enlarged the app slots of most Gen3 and Pro
//! models), so a package with another layout is refused until its profile
//! is updated. The `min_version` and the approach come from
//! [Free-Shelly OTA](https://github.com/oxynatOr/free-shelly-ota)
//! (Apache-2.0), which found them on real devices.

use crate::{esp32::ChipId, stock_package::StockPackage};

/// Version of a bootloader as the stock installer compares it
/// (`major.minor.patch`; the update log prints 1.0.3 as `010003ff`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BootloaderVersion {
    major: u8,
    minor: u8,
    patch: u8,
}

impl std::str::FromStr for BootloaderVersion {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut numbers = text.split('.').map(str::parse::<u8>);
        match (numbers.next(), numbers.next(), numbers.next(), numbers.next()) {
            (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
                Ok(Self { major, minor, patch })
            }
            _ => Err(format!("{text:?} is no bootloader version")),
        }
    }
}

impl BootloaderVersion {
    /// The version in the update log's form, `010003ff` for 1.0.3: major,
    /// minor and patch as hex bytes, then a byte that is not part of it.
    #[must_use]
    pub fn from_log_word(word: &str) -> Option<Self> {
        let value = Some(word)
            .filter(|word| word.len() == 8 && word.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .and_then(|word| u32::from_str_radix(word, 16).ok())?;
        let [major, minor, patch, _] = value.to_be_bytes();
        Some(Self { major, minor, patch })
    }
}

impl std::fmt::Display for BootloaderVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The `min_version` an ESPHome package gives its bootloader. The installer
/// writes the bootloader only if `min_version` is higher than the version it
/// considers installed. Devices report 1.0.2 or 1.0.3 there (the official
/// packages carry 1.0.2 or 1.0.3), so one patch level above the package is
/// not enough on every model: the H&T Gen3 skipped 1.0.3 (`update? 0`), and
/// ESPHome then ran without its bootloader. 1.0.9 was written on a Plug M
/// Gen3 (`Boot: cur 010002ff, ..., min 010009ff, update? 1`) and is the value
/// of every profile.
pub const BOOT_MIN_VERSION: BootloaderVersion = BootloaderVersion { major: 1, minor: 0, patch: 9 };

/// The flash layout of one app's current stable firmware. App name and
/// firmware version are plain text: `AppName` and `FirmwareVersion` hold a
/// `String` and cannot be built in a `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    app: &'static str,
    firmware: &'static str,
    chip: ChipId,
    table_address: usize,
    app_slot_size: usize,
}

const ESP32: ChipId = ChipId(0x0000);
const ESP32_C3: ChipId = ChipId(0x0005);

/// `(app, firmware, chip, table address, app slot size)` as a [`Profile`]. A
/// macro rather than a `const fn`: a `const fn` evaluated only at compile
/// time never runs under coverage instrumentation and counts as uncovered.
macro_rules! profiles {
    ($(($app:literal, $firmware:literal, $chip:ident, $table:literal, $slot:literal)),* $(,)?) => {
        [$(Profile {
            app: $app,
            firmware: $firmware,
            chip: $chip,
            table_address: $table,
            app_slot_size: $slot,
        }),*]
    };
}

/// Every app with a profile, read from the current stable package of the
/// update service on 2026-11-12. Gen4 (ESP32-C6) is out of scope, and
/// `RGBCCTBulbG3` has no stable package.
pub const PROFILES: [Profile; 42] = profiles![
    ("Dimmer0110VPMG3", "2.2.0", ESP32_C3, 0xF000, 0x30_0000),
    ("DimmerG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("DuoBulbG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("EMG3", "2.2.0", ESP32_C3, 0x1_0000, 0x28_0000),
    ("HTG3", "2.0.3", ESP32_C3, 0xF000, 0x28_0000),
    ("I4G3", "2.2.0", ESP32_C3, 0xF000, 0x30_0000),
    ("Mini1G3", "2.2.0", ESP32_C3, 0xF000, 0x30_0000),
    ("Mini1PMG3", "2.2.0", ESP32_C3, 0xF000, 0x30_0000),
    ("MiniPMG3", "2.2.0", ESP32_C3, 0xF000, 0x30_0000),
    ("OutdoorPlugSG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("Pill", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("PlugAZ", "2.2.0", ESP32_C3, 0xF000, 0x30_0000),
    ("PlugMG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("PlugPMG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("PlugSG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("PlugUS", "1.7.6", ESP32, 0x8000, 0x19_0000),
    ("Plus1", "1.7.6", ESP32, 0x8000, 0x19_0000),
    ("Plus1Mini", "1.7.6", ESP32_C3, 0x8000, 0x19_0000),
    ("Plus1PM", "1.7.6", ESP32, 0x8000, 0x19_0000),
    ("Plus1PMMini", "1.7.6", ESP32_C3, 0x8000, 0x19_0000),
    ("Plus2PM", "1.7.6", ESP32, 0x8000, 0x19_0000),
    ("PlusI4", "1.7.6", ESP32, 0x8000, 0x19_0000),
    ("PlusPMMini", "1.7.6", ESP32_C3, 0x8000, 0x19_0000),
    ("PlusPlugIT", "2.2.0", ESP32, 0x8000, 0x19_0000),
    ("PlusPlugS", "1.7.6", ESP32, 0x8000, 0x19_0000),
    ("PlusPlugUK", "2.2.0", ESP32, 0x8000, 0x19_0000),
    ("PlusRGBWPM", "2.2.0", ESP32, 0x8000, 0x19_0000),
    ("PlusUni", "2.2.0", ESP32, 0x8000, 0x19_0000),
    ("Pro1", "2.2.0", ESP32, 0x8000, 0x30_0000),
    ("Pro1PM", "2.2.0", ESP32, 0x8000, 0x30_0000),
    ("Pro2", "2.2.0", ESP32, 0x8000, 0x30_0000),
    ("Pro2PM", "2.2.0", ESP32, 0x8000, 0x30_0000),
    ("Pro3", "2.2.0", ESP32, 0x8000, 0x30_0000),
    ("Pro3EM", "2.2.0", ESP32, 0x8000, 0x20_0000),
    ("Pro4PM", "2.2.0", ESP32, 0x8000, 0x30_0000),
    ("ProRGBWWPM", "2.2.0", ESP32, 0x8000, 0x30_0000),
    ("S1G3", "2.2.0", ESP32_C3, 0xF000, 0x30_0000),
    ("S1LG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("S1PMG3", "2.2.0", ESP32_C3, 0xF000, 0x30_0000),
    ("S2LG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("S2PMG3", "2.2.0", ESP32_C3, 0x1_0000, 0x30_0000),
    ("S3EMG3", "2.2.0", ESP32_C3, 0x1_0000, 0x28_0000),
];

/// The partition the official package writes the application to and the
/// stock installer starts ESPHome from (see `esp32::factory::APP_PARTITION`).
const APP_SLOT: &str = crate::esp32::factory::APP_PARTITION;

/// Reasons why no ESPHome package is built for an official package.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// No profile names the app.
    #[error(
        "{0} has no profile: its flash layout and bootloader have not been checked for a \
         migration yet"
    )]
    Unknown(String),
    /// The package's layout differs from the profile.
    #[error(
        "the official package {app} {version} has another flash layout than {app} {known}, the \
         one this tool knows ({}); Shelly changes the layout between releases, and a new one \
         needs a new profile",
        differences.join("; ")
    )]
    Layout {
        /// App name.
        app: String,
        /// Firmware version of the package.
        version: String,
        /// Firmware version the profile was read from.
        known: &'static str,
        /// What differs.
        differences: Vec<String>,
    },
}

impl Profile {
    /// The app the profile is for, e.g. `S1G3`.
    #[must_use]
    pub fn app(&self) -> &'static str {
        self.app
    }

    /// The profile of `app`, if there is one.
    #[must_use]
    pub fn by_app(app: &str) -> Option<&'static Self> {
        PROFILES.iter().find(|profile| profile.app == app)
    }

    /// The profile of `package`'s app, if the package has the layout it
    /// describes: the chip, the address of the partition table, the size of
    /// the application slot and the slot the manifest writes to.
    ///
    /// # Errors
    /// A [`ProfileError`] if the app has no profile or another layout.
    pub fn of(package: &StockPackage) -> Result<&'static Self, ProfileError> {
        let app = package.name().as_str();
        let profile = Self::by_app(app).ok_or_else(|| ProfileError::Unknown(app.to_owned()))?;
        let mut differences = Vec::new();
        if package.app().chip != profile.chip {
            differences.push(format!("chip {}, expected {}", package.app().chip, profile.chip));
        }
        let (_, table_address) = package.addresses();
        if table_address != profile.table_address {
            differences.push(format!(
                "partition table at {table_address:#x}, expected {:#x}",
                profile.table_address
            ));
        }
        match package.partition_table().find(APP_SLOT) {
            Some(slot) if slot.size == profile.app_slot_size => {}
            Some(slot) => differences.push(format!(
                "{APP_SLOT} of {:#x} bytes, expected {:#x}",
                slot.size, profile.app_slot_size
            )),
            None => differences.push(format!("no {APP_SLOT}")),
        }
        let target = package.part("app").and_then(|part| part.ptn.as_deref());
        if target != Some(APP_SLOT) {
            differences.push(format!(
                "the application goes to {}, expected {APP_SLOT}",
                target.unwrap_or_default()
            ));
        }
        if differences.is_empty() {
            Ok(profile)
        } else {
            Err(ProfileError::Layout {
                app: app.to_owned(),
                version: package.version().to_string(),
                known: profile.firmware,
                differences,
            })
        }
    }
}

#[cfg(test)]
#[path = "unit_tests/profile.rs"]
mod tests;
