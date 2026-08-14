#![no_main]

#[path = "../postgres_stubs.rs"]
mod postgres_stubs;

use graph::fuzz_support::{edge_type_dictionary_seed_bytes, load_edge_type_dictionary};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.first() == Some(&b'D') {
        if let Some(mut seed) = edge_type_dictionary_seed_bytes("basic") {
            for (index, byte) in data[1..].iter().filter(|byte| **byte != b'\n').enumerate() {
                let offset = index % seed.len();
                seed[offset] ^= *byte;
            }
            let _ = load_edge_type_dictionary(&seed);
        }
    }
    if let Ok(name) = std::str::from_utf8(data) {
        if let Some(seed) = edge_type_dictionary_seed_bytes(name) {
            let _ = load_edge_type_dictionary(&seed);
        }
    }
    let _ = load_edge_type_dictionary(data);
});
