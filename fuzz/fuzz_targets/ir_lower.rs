//! Decode arbitrary bytes as a gvir Package and lower it. Both stages
//! must reject or degrade — never panic (parent spec §12.4).

#![no_main]

use libfuzzer_sys::fuzz_target;
use prost::Message;

fuzz_target!(|data: &[u8]| {
    if let Ok(pkg) = goverify_extract::gvir::Package::decode(data) {
        let _ = goverify_ir::Program::from_packages(vec![pkg]);
    }
});
