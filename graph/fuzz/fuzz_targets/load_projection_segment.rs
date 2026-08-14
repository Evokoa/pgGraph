#![no_main]

#[path = "../postgres_stubs.rs"]
mod postgres_stubs;

use graph::fuzz_support::{load_projection_segment, projection_segment_seed_bytes};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let seed_name = match data.first() {
        Some(b'1') => Some("edge_one"),
        Some(b'2') => Some("edge_two"),
        Some(b'4') => Some("edge_four"),
        Some(b'6') => Some("edge_v6"),
        _ => None,
    };
    if let Some(seed_name) = seed_name {
        if let Some(mut seed) = projection_segment_seed_bytes(seed_name) {
            for (index, byte) in data[1..].iter().filter(|byte| **byte != b'\n').enumerate() {
                let offset = index % seed.len();
                seed[offset] ^= *byte;
            }
            let _ = load_projection_segment(&seed);
        }
    }
    if let Ok(name) = std::str::from_utf8(data) {
        if let Some(seed) = projection_segment_seed_bytes(name) {
            let _ = load_projection_segment(&seed);
        }
    }
    let _ = load_projection_segment(data);
});
