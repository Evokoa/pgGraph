#![no_main]

#[path = "../postgres_stubs.rs"]
mod postgres_stubs;

use graph::fuzz_support;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    let _ = fuzz_support::parse_sync_properties(Some(input));
});
