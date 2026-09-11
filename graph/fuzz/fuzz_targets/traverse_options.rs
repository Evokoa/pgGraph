#![no_main]

#[path = "../postgres_stubs.rs"]
mod postgres_stubs;

use graph::fuzz_support;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let parts = input.split('\0').collect::<Vec<_>>();
    let direction = parts.first().copied().unwrap_or_default();
    let strategy = parts.get(1).copied().unwrap_or_default();
    let uniqueness = parts.get(2).copied().unwrap_or_default();

    let _ = fuzz_support::validate_traverse_options(direction, strategy, uniqueness);
});
