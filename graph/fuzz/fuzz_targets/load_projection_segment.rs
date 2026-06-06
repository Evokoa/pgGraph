#![no_main]

use graph::fuzz_support::{load_projection_segment, projection_segment_seed_bytes};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(name) = std::str::from_utf8(data) {
        if let Some(seed) = projection_segment_seed_bytes(name) {
            let _ = load_projection_segment(&seed);
        }
    }
    let _ = load_projection_segment(data);
});
