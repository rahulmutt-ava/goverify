//! Parse arbitrary bytes as a baseline file. The baseline is
//! user-editable gate configuration — the parser must reject, never
//! panic (phase-5b spec §4; parent spec §12.4).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = goverify_cli::baseline::parse(data);
});
