//! End-to-end compile_program test against the extracted "hello" corpus
//! (phase-6 spec §3, Task 6): a real `.gvir`-lowered `Program`, not a
//! hand-built fixture, so this also exercises `Program::pragmas` (Task
//! 2) and `parse_pragma` (Task 5) wired together through `compile_one`.

use goverify_analysis::annotations::CONTRACT;
use goverify_ir::testutil::load_corpus;

#[test]
fn hello_requires_compiles_to_nonnil_clause() {
    let p = load_corpus("hello");
    let ann = goverify_spec::compile_program(&p, &["nil", "bounds"]);
    let f = p.lookup_func("example.com/hello.Deref").unwrap();
    let fa = ann.funcs.get(&f).expect("Deref annotations");
    assert_eq!(fa.requires.len(), 1);
    assert_eq!(fa.requires[0].clause.tag, CONTRACT);
    assert_eq!(fa.requires[0].text, "p != nil");
    // Free vars: exactly p0.
    let vars = fa.requires[0].clause.formula.term.free_vars();
    assert_eq!(vars.keys().collect::<Vec<_>>(), vec!["p0"]);
    assert!(ann.findings.is_empty(), "hello has no bad annotations");
}
