//! Unit tests of `crate::flash_esp32` (not counted as production code for coverage).

use super::*;

#[test]
fn explains_a_refused_update() {
    let refused = |message: &str| {
        refusal(RpcError::Refused {
            method: "Shelly.Update".into(),
            code: -109,
            message: message.into(),
        })
        .to_string()
    };
    assert!(refused("Not committed").contains("run flash again in two minutes"));
    assert_eq!(
        refused("Low battery"),
        "the device refused the update: Low battery. The firmware was not changed; the device log \
         setting is put back."
    );
    assert!(matches!(refusal(RpcError::AuthFailed), FlashError::Rpc(RpcError::AuthFailed)));
}
