//! Curated golden dumps (phase-2 spec §9.4). Byte-exact. Regenerate with
//! UPDATE_GOLDENS=1 after intentional lowering/dump changes and review
//! the diff by hand.

use goverify_ir::{dump_function, testutil};

fn dump_module(module: &str, import_path: &str) -> String {
    let p = testutil::load_corpus(module);
    let mut s = String::new();
    for f in p.func_ids() {
        // Golden covers only the module's own package — stdlib dumps
        // would churn with Go toolchain bumps.
        if p.func(f).is_some() && p.func_name(f).contains(import_path) {
            s.push_str(&dump_function(&p, f));
            s.push('\n');
        }
    }
    s
}

#[test]
fn hello_ir_matches_golden() {
    testutil::check_golden("hello.ir.txt", &dump_module("hello", "example.com/hello"));
}
