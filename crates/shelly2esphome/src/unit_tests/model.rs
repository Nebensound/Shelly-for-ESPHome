//! Unit tests of `crate::model` (not counted as production code for coverage).

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
