#![no_main]

use graph::fuzz_support::load_projection_manifest;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(raw) = std::str::from_utf8(data) {
        let _ = load_projection_manifest(raw);
    }
});
