#![no_main]

#[path = "../postgres_stubs.rs"]
mod postgres_stubs;

use graph::fuzz_support::load_graph_file;
use libfuzzer_sys::fuzz_target;
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    let path = std::env::temp_dir().join(format!(
        "graph-fuzz-load-{}-{:p}.pggraph",
        std::process::id(),
        data.as_ptr()
    ));

    if let Ok(mut file) = std::fs::File::create(&path) {
        let _ = file.write_all(data);
        let _ = file.flush();
        let _ = load_graph_file(&path);
    }
    let _ = std::fs::remove_file(&path);
});
