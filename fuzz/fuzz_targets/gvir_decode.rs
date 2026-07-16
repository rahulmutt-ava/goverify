//! The .gvir decoder parses bytes the analyzer didn't write (shared
//! caches, spec §14): it must reject malformed input, never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use prost::Message;

fuzz_target!(|data: &[u8]| {
    let _ = goverify_extract::gvir::Package::decode(data);
});
