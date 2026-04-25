//! Regression test for the iid collision bug. The unpatched
//! `LightbulbAccessory::new` (in the ihciah hap-rs fork) places brightness
//! and firmware_revision both at iid=10 within the same accessory, which
//! violates the HAP spec — iOS responds with "this accessory cannot be used
//! with HomeKit" right after pair-setup. `homekit::fix_lightbulb_iids`
//! shifts the lightbulb service to clear the collision; this test pins
//! that behavior.

use hap::{
    accessory::{AccessoryInformation, lightbulb::LightbulbAccessory},
    service::HapService,
};

fn build_bulb() -> LightbulbAccessory {
    LightbulbAccessory::new(
        1,
        AccessoryInformation {
            name: "test".into(),
            manufacturer: "led_remote".into(),
            model: "RM12 Bridge".into(),
            serial_number: "LRRM12-AABBCC".into(),
            firmware_revision: Some("0.1.0".into()),
            ..Default::default()
        },
    )
    .unwrap()
}

fn collect_iids(bulb: &LightbulbAccessory) -> Vec<u64> {
    let mut iids = Vec::new();
    for c in bulb.accessory_information.get_characteristics() {
        iids.push(c.get_id());
    }
    for c in bulb.lightbulb.get_characteristics() {
        iids.push(c.get_id());
    }
    iids
}

fn dupes(mut iids: Vec<u64>) -> Vec<u64> {
    iids.sort();
    iids.windows(2).filter(|w| w[0] == w[1]).map(|w| w[0]).collect()
}

#[test]
fn unpatched_constructor_has_iid_collision() {
    // Documents the upstream bug we're working around. If this ever starts
    // passing, the fork was fixed and `fix_lightbulb_iids` can go away.
    let bulb = build_bulb();
    let dupes = dupes(collect_iids(&bulb));
    assert_eq!(dupes, vec![10], "expected upstream collision at iid=10");
}

#[test]
fn fix_lightbulb_iids_clears_collision() {
    let mut bulb = build_bulb();
    led_remote::homekit::fix_lightbulb_iids(&mut bulb);
    let dupes = dupes(collect_iids(&bulb));
    assert!(dupes.is_empty(), "duplicate iids after fix: {dupes:?}");
}
