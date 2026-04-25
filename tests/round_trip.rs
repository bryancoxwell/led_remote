//! Cross-validate Rust pipeline against the actual captures: load each capture,
//! decode its packets via the Rust slicer, and assert they match
//! `build_packet(Button, X)` for the expected counter range.

use std::path::PathBuf;

use led_remote::{
    Button, build_packet, decode_packets, read_capture, run_lengths, slice_envelope,
};

fn captures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("captures")
}

#[test]
fn decodes_each_capture_to_expected_packets() {
    // Each capture has 5 button presses, each emitting N identical packet
    // repetitions; the press counter X increments by 1 between presses.
    // (turn_on starts at X=1 because its capture begins mid-stream.)
    let cases = [
        (Button::TurnOn, 5u8, 1u8),          // 5 reps/press, starts at X=1
        (Button::TurnOff, 3, 0),
        (Button::TemperatureDown, 3, 0),
        (Button::TemperatureUp, 3, 0),
        (Button::BrightnessDown, 3, 0),
    ];

    for (btn, expected_reps, start_x) in cases {
        let cap = read_capture(btn.name(), captures_dir())
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", btn.name()));
        let binary = slice_envelope(&cap.samples, cap.sample_rate, 20.0, 0.30);
        let runs = run_lengths(&binary);
        let packets = decode_packets(&runs, cap.sample_rate);

        // 5 button presses × expected_reps repetitions per press
        assert_eq!(
            packets.len(),
            (5 * expected_reps as usize),
            "{}: unexpected packet count",
            btn.name()
        );

        // Group consecutive duplicates and verify each press's bits match build_packet.
        let mut groups: Vec<(u64, usize)> = Vec::new();
        for &p in &packets {
            match groups.last_mut() {
                Some((last, n)) if *last == p => *n += 1,
                _ => groups.push((p, 1)),
            }
        }
        assert_eq!(groups.len(), 5, "{}: expected 5 distinct presses", btn.name());

        for (i, (bits, count)) in groups.iter().enumerate() {
            let x = start_x.wrapping_add(i as u8);
            let expected = build_packet(btn, x);
            assert_eq!(
                *bits,
                expected,
                "{}: press {} bits 0x{:010X} != expected 0x{:010X} (X={})",
                btn.name(),
                i,
                bits,
                expected,
                x
            );
            assert_eq!(*count, expected_reps as usize, "{}: press {} rep count", btn.name(), i);
        }
    }
}
