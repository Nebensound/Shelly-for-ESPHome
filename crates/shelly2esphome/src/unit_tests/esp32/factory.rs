//! Unit tests of `crate::esp32::factory` (not counted as production code for coverage).

use super::*;
use crate::esp32::synthetic::{Fixture, ImageSpec, LAYOUT, image, table};

fn package(fixture: &Fixture) -> StockPackage {
    StockPackage::parse(&fixture.package()).expect("package")
}

fn check(fixture: &Fixture) -> Result<(FactoryImage, Vec<String>), FactoryError> {
    FactoryImage::check(fixture.factory(), &package(fixture))
}

#[test]
fn accepts_a_matching_factory_image() {
    let fixture = Fixture::default();
    let (factory, warnings) = check(&fixture).expect("matching");
    assert!(warnings.is_empty());
    assert_eq!(factory.bootloader_bytes(), fixture.bootloader.as_slice());
    assert_eq!(factory.app_bytes(), fixture.app.as_slice());
    assert!(factory.app().is_app);
    assert!(factory.partition_table().find("app_0").is_some());

    // A differing non-critical partition only warns.
    let mut other = Fixture::default();
    let mut layout = LAYOUT.to_vec();
    layout.retain(|entry| entry.0 != "shelly");
    other.table = table(&layout);
    let (_, warnings) = check(&other).expect("only a warning");
    assert_eq!(warnings, ["shelly: missing, the device has it"]);
}

#[test]
fn refuses_factory_images_that_do_not_suit_the_device() {
    let error = |change: &dyn Fn(&mut Fixture)| {
        let mut fixture = Fixture::default();
        change(&mut fixture);
        check(&fixture).expect_err("must fail")
    };
    assert!(matches!(
        error(&|fixture| fixture.bootloader[0] = 0),
        FactoryError::Bootloader { address: 0, source: ImageError::NotAnImage(0) }
    ));
    assert!(matches!(
        error(&|fixture| fixture.table[0] = 0),
        FactoryError::PartitionTable { address: 0x8000, .. }
    ));
    let without = |label: &'static str| {
        move |fixture: &mut Fixture| {
            let mut layout = LAYOUT.to_vec();
            layout.retain(|entry| entry.0 != label);
            fixture.table = table(&layout);
        }
    };
    assert_eq!(error(&without("otadata")), FactoryError::MissingPartition("otadata"));
    assert_eq!(error(&without("app_0")), FactoryError::MissingPartition("app_0"));
    assert!(matches!(error(&|fixture| fixture.app[0] = 0), FactoryError::App(_)));
    assert_eq!(error(&|fixture| fixture.app = image(ImageSpec::default())), FactoryError::NotAnApp);
    let big = image(ImageSpec { app: true, payload: 0x1_0000, ..ImageSpec::default() });
    assert!(matches!(
        error(&|fixture| fixture.app = big.clone()),
        FactoryError::AppTooLarge { capacity: 0x1_0000, .. }
    ));
    let mut moved = LAYOUT.to_vec();
    moved[1].3 = 0xC000; // nvs at another offset
    let critical = error(&|fixture| fixture.table = table(&moved));
    assert!(critical.to_string().starts_with("the partition table differs from the device's: nvs"));
    for (spec, field) in [
        (ImageSpec { chip: 0, ..ImageSpec::default() }, "chip"),
        (ImageSpec { mode: 3, ..ImageSpec::default() }, "flash mode"),
        (
            ImageSpec { size_and_frequency: 0x1F, ..ImageSpec::default() },
            "flash size and frequency",
        ),
        (ImageSpec { min_chip_revision: 0, ..ImageSpec::default() }, "minimum chip revision"),
    ] {
        assert_eq!(
            error(&|fixture| fixture.bootloader = image(spec)),
            FactoryError::BootloaderMismatch(field)
        );
    }
    let c6 = image(ImageSpec { chip: 13, app: true, payload: 4096, ..ImageSpec::default() });
    let chip = error(&|fixture| fixture.app = c6.clone());
    assert_eq!(chip.to_string(), "the firmware is built for ESP32-C6, the device has ESP32-C3");
}

#[test]
fn refuses_a_used_otadata_area() {
    let fixture = Fixture::default();
    let mut factory = fixture.factory();
    factory[0x9000] = 0x00;
    let error = FactoryImage::check(factory, &package(&fixture)).expect_err("used otadata");
    assert_eq!(error, FactoryError::OtadataNotErased);
}
