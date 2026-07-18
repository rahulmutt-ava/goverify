//! Decode arbitrary bytes as a gvir Package, lower, and encode every
//! function to its gated-SSA SMT form. All three stages must reject or
//! degrade — never panic (parent spec §12.4).

#![no_main]

use libfuzzer_sys::fuzz_target;
use prost::Message;

fuzz_target!(|data: &[u8]| {
    if let Ok(pkg) = goverify_extract::gvir::Package::decode(data) {
        let p = goverify_ir::Program::from_packages(vec![pkg]);
        for f in p.func_ids() {
            if p.func(f).is_none() {
                continue;
            }
            let _ = goverify_analysis::encode_func(&p, f);
        }
    }
});
