//! Shelly Gen1 models supported by the migration firmware.
//!
//! The table mirrors the model conditions in `migration_firmware/mos.yml`. The
//! application name is what the stock firmware compares against the `name` in
//! the OTA package manifest, so a package built for another model is rejected.

/// A Shelly Gen1 device model the migration firmware can be built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Model {
    /// Shelly 1 (`SHSW-1`).
    Shelly1,
    /// Shelly 1PM (`SHSW-PM`).
    Shelly1Pm,
    /// Shelly 1L (`SHSW-L`).
    Shelly1L,
    /// Shelly 2 (`SHSW-21`).
    Shelly2,
    /// Shelly 2.5 (`SHSW-25`).
    Shelly25,
    /// Shelly Plug S (`SHPLG-S`).
    ShellyPlugS,
    /// Shelly Plug US (`SHPLG-U1`).
    ShellyPlugUs,
    /// Shelly RGBW2 in colour mode (`SHRGBW2`).
    ShellyRgbw2,
    /// Shelly Dimmer 1 (`SHDM-1`).
    ShellyDimmer1,
    /// Shelly Dimmer 2 (`SHDM-2`).
    ShellyDimmer2,
    /// Shelly EM (`SHEM`).
    ShellyEm,
    /// Shelly Bulb (`SHBLB-1`).
    ShellyBulb,
    /// Shelly Vintage (`SHVIN-1`).
    ShellyVintage,
    /// Shelly Duo (`SHBDUO-1`).
    ShellyDuo,
    /// Shelly Duo RGBW / colour bulb (`SHCB-1`).
    ShellyDuoRgbw,
    /// Shelly i3 (`SHIX3-1`).
    ShellyI3,
    /// Shelly UNI (`SHUNI-1`).
    ShellyUni,
}

/// (model, device type reported by `/shelly`, build name, application name)
const TABLE: [(Model, &str, &str, &str); 17] = [
    (Model::Shelly1, "SHSW-1", "Shelly1", "switch1"),
    (Model::Shelly1Pm, "SHSW-PM", "Shelly1PM", "switch1pm"),
    (Model::Shelly1L, "SHSW-L", "Shelly1L", "switch1l"),
    (Model::Shelly2, "SHSW-21", "Shelly2", "switch"),
    (Model::Shelly25, "SHSW-25", "Shelly25", "switch25"),
    (Model::ShellyPlugS, "SHPLG-S", "ShellyPlugS", "shelly-plug-s"),
    (Model::ShellyPlugUs, "SHPLG-U1", "ShellyPlugUS", "shelly-plug-u1"),
    (Model::ShellyRgbw2, "SHRGBW2", "ShellyRGBW2", "rgbw2-color"),
    (Model::ShellyDimmer1, "SHDM-1", "ShellyDimmer1", "dimmer"),
    (Model::ShellyDimmer2, "SHDM-2", "ShellyDimmer2", "dimmer-l51"),
    (Model::ShellyEm, "SHEM", "ShellyEM", "shellyem"),
    (Model::ShellyBulb, "SHBLB-1", "ShellyBulb", "bulb"),
    (Model::ShellyVintage, "SHVIN-1", "ShellyVintage", "bulb6w"),
    (Model::ShellyDuo, "SHBDUO-1", "ShellyDuo", "bulbduo"),
    (Model::ShellyDuoRgbw, "SHCB-1", "ShellyDuoRGBW", "color-bulb"),
    (Model::ShellyI3, "SHIX3-1", "ShellyI3", "ix3"),
    (Model::ShellyUni, "SHUNI-1", "ShellyUni", "uni"),
];

impl Model {
    /// Maps the `type` field of the stock firmware's `/shelly` endpoint.
    #[must_use]
    pub fn from_device_type(device_type: &str) -> Option<Self> {
        TABLE.iter().find(|(_, ty, _, _)| *ty == device_type).map(|(model, _, _, _)| *model)
    }

    fn row(self) -> &'static (Model, &'static str, &'static str, &'static str) {
        TABLE
            .iter()
            .find(|(model, _, _, _)| *model == self)
            .expect("every Model variant has exactly one row in TABLE")
    }

    /// Device type as reported by the stock firmware, e.g. `SHDM-2`.
    #[must_use]
    pub fn device_type(self) -> &'static str {
        self.row().1
    }

    /// `MODEL` build variable of the migration firmware, e.g. `ShellyDimmer2`.
    #[must_use]
    pub fn build_name(self) -> &'static str {
        self.row().2
    }

    /// Application name the stock firmware expects in the OTA manifest.
    #[must_use]
    pub fn app_name(self) -> &'static str {
        self.row().3
    }

    /// Largest image the migration firmware can stage on this model.
    ///
    /// All supported models have 2 MiB flash with two rboot slots of 1 MiB;
    /// the staging area spans from slot 0 (at 0x8000) to slot 1. The value is
    /// deliberately conservative (one slot minus the slot offset and six
    /// sectors); the migration firmware reports its exact limit as well.
    #[must_use]
    pub fn max_image_bytes(self) -> usize {
        const SLOT_BYTES: usize = 0x10_0000;
        const SLOT_OFFSET: usize = 0x8000;
        const RESERVED_SECTORS: usize = 6;
        SLOT_BYTES - SLOT_OFFSET - RESERVED_SECTORS * 4096
    }

    /// File name of the migration package published by CI.
    #[must_use]
    pub fn migration_asset_name(self) -> String {
        format!("mg2esphome-{}.zip", self.build_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_round_trips_through_its_device_type() {
        for (model, device_type, _, _) in TABLE {
            assert_eq!(Model::from_device_type(device_type), Some(model));
            assert_eq!(model.device_type(), device_type);
        }
    }

    #[test]
    fn dimmer2_uses_the_stock_application_name() {
        let model = Model::ShellyDimmer2;
        assert_eq!(model.build_name(), "ShellyDimmer2");
        assert_eq!(model.app_name(), "dimmer-l51");
        assert_eq!(model.migration_asset_name(), "mg2esphome-ShellyDimmer2.zip");
    }

    #[test]
    fn image_limit_leaves_room_for_scratch_sectors() {
        assert_eq!(Model::Shelly1Pm.max_image_bytes(), 991_232);
    }

    #[test]
    fn unsupported_types_are_rejected() {
        // Battery powered and 4 MB flash devices are intentionally not supported.
        assert_eq!(Model::from_device_type("SHHT-1"), None);
        assert_eq!(Model::from_device_type("SHPLG2-1"), None);
        assert_eq!(Model::from_device_type(""), None);
    }
}
