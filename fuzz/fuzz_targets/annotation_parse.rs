//! Parse arbitrary bytes as a //goverify: pragma line. Annotation text
//! is repo-authored but untrusted (parent spec §11; phase-6 spec §7) —
//! the parser must reject, never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = goverify_spec::parse::parse_pragma(s);
    }
});
