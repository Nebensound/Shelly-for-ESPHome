//! Unit tests of `crate::report` (not counted as production code for coverage).

use super::*;

fn full() -> Report {
    Report {
        result: SUCCESS.to_owned(),
        facts: DeviceFacts {
            model: Some(Model::Shelly25),
            stock_firmware: Some("20230913-114008/v1.14.0-gcb84623".into()),
            rssi_dbm: Some(-67),
            login: Some(true),
            backup: Some(false),
            package: Some(PackageSource::Embedded),
        },
        duration: Duration::from_millis(94_700),
    }
}

#[test]
fn fills_every_field_and_marks_unknowns() {
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let fields = full().fields();
    let expected = [
        ("result", "success"),
        ("model", "SHSW-25"),
        ("generation", "Gen1"),
        ("stock-firmware", "20230913-114008/v1.14.0-gcb84623"),
        ("signal", "-67 dBm"),
        ("login", "yes"),
        ("backup", "no"),
        ("package", "embedded"),
        ("tool", env!("CARGO_PKG_VERSION")),
        ("platform", &platform),
        ("duration", "94 s"),
    ];
    assert_eq!(fields, expected.map(|(id, value)| (id, value.to_owned())));

    let empty = Report {
        result: "device-unreachable".into(),
        facts: DeviceFacts { package: Some(PackageSource::File), ..DeviceFacts::default() },
        duration: Duration::ZERO,
    };
    let values: Vec<String> = empty.fields().into_iter().map(|(_, value)| value).collect();
    assert_eq!(
        values[..8],
        [
            "device-unreachable",
            "unknown",
            "Gen1",
            "unknown",
            "unknown",
            "unknown",
            "unknown",
            "file"
        ]
    );
    let no_package = Report { facts: DeviceFacts::default(), ..empty };
    assert_eq!(no_package.fields()[7].1, "unknown");
}

#[test]
fn codes_the_fields_so_edits_show() {
    let fields = full().fields();
    let lines: String = fields.iter().map(|(id, value)| format!("{id}={value}\n")).collect();
    let digest = hex::encode(Sha256::digest(format!("{CODE_SALT}\n{lines}")));
    assert_eq!(Report::code(&fields), digest[..16]);
    let mut edited = fields.clone();
    edited[4].1 = "-40 dBm".into();
    assert_ne!(Report::code(&edited), Report::code(&fields));
}

#[test]
fn links_to_the_form_of_the_result() {
    let success = full().url();
    assert!(success.starts_with(
        "https://github.com/Nebensound/Shelly-for-ESPHome/issues/new?template=device-report.yml\
         &title=Device%20report%3A%20SHSW-25%20works&result=success&model=SHSW-25&"
    ));
    assert!(success.contains("&stock-firmware=20230913-114008%2Fv1.14.0-gcb84623&"));
    assert!(success.contains("&signal=-67%20dBm&"));
    assert!(success.ends_with(&format!("&report-code={}", Report::code(&full().fields()))));

    let failure = Report {
        result: "device-failed:size_mismatch".into(),
        facts: DeviceFacts::default(),
        ..full()
    };
    assert!(failure.url().contains(
        "?template=device-failure.yml&title=Flash%20failed%3A%20unknown%20device%20\
         %28device-failed%3Asize_mismatch%29&result=device-failed%3Asize_mismatch&"
    ));
}

#[test]
fn encodes_all_but_unreserved_characters() {
    assert_eq!(encode("aZ09-_.~"), "aZ09-_.~");
    assert_eq!(encode("a b/ä&="), "a%20b%2F%C3%A4%26%3D");
}
