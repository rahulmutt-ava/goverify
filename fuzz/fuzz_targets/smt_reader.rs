//! The reader parses solver output — bytes the analyzer didn't write.
//! It must reject, never panic (parent spec §12.4).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = goverify_solver::parse_sexpr(s);
        let _ = goverify_solver::parse_query(s);
        let _ = goverify_solver::parse_response(s);
    }
});
