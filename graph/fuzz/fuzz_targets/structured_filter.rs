#![no_main]

#[path = "../postgres_stubs.rs"]
mod postgres_stubs;

use graph::fuzz_support;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return;
    };

    if let serde_json::Value::Object(map) = &value {
        for (operator, operand) in map {
            let _ = fuzz_support::validate_structured_operator_shape(operator, operand);
        }
    }
});
