//! End-to-end annotation-language corpus (phase-6 spec, Task 11): the
//! `annot` module is the extractor/IR/compiler/engine regression surface
//! for the whole `//goverify:` pragma pipeline (Tasks 1-10) — pragma
//! extraction + result-name resolution, `parse_pragma`/`compile_program`
//! (bad-annotation diagnostics), the engine's merged-summary contract
//! obligations and `unverified-annotation` best-effort ensures
//! verification, and the `ignores` metadata the CLI's pragma-ignore
//! filter consumes downstream (that filter itself is CLI-only — see
//! `annot_integration.rs` — so `Suppressed`'s ignored finding is still
//! present, and pinned, at this engine layer).

use std::collections::BTreeSet;

use goverify_analysis::{
    CONTRACT, EngineConfig, Finding, Options, Provenance, Severity, UNVERIFIED_ANNOTATION,
    analyze_full,
};
use goverify_checkers::default_checkers;
use goverify_ir::Program;
use goverify_solver::{SolverLimits, Z3Native};
use goverify_spec::{ANNOTATION_VERSION, compile_program};

/// Same `ignore`-name universe the CLI computes (`default_checkers`'
/// names + the three annotation finding classes) — `BadIgnore`'s
/// `no-such-checker` must stay unknown against it.
const KNOWN: &[&str] = &[
    "nil",
    "bounds",
    "contract",
    "bad-annotation",
    "unverified-annotation",
];

fn limits() -> SolverLimits {
    // Corpus queries are trivial; generous timeout so slow CI can't turn
    // a Sat into Unknown and flake the assertions (nil_corpus.rs
    // convention).
    SolverLimits {
        timeout_ms: 5_000,
        mem_mb: 1024,
    }
}

/// Extract, compile annotations, and run the full engine with the real
/// checker set + a Z3 backend — mirrors `nil_corpus.rs`'s harness, plus
/// `compile_program`'s output feeding `EngineConfig.annotations`
/// (phase-6 spec §4). Returns the loaded program, the compiled
/// annotations (kept around for pragma-position assertions the engine
/// output alone can't reconstruct), and the analysis.
fn analyze() -> (
    Program,
    goverify_analysis::Annotations,
    goverify_analysis::Analysis,
) {
    let p = goverify_ir::testutil::load_corpus("annot");
    let ann = compile_program(&p, KNOWN);
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: None,
        emit_smt: None,
        annotations: ann.clone(),
        annotation_version: ANNOTATION_VERSION,
    };
    let checkers = default_checkers();
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    (p, ann, a)
}

/// bad.go carries 8 malformed pragmas, one per bad-annotation finding
/// class (unknown name, result-in-requires, field selection, effects,
/// pure, unknown directive, unknown ignore checker, parse error) —
/// bespoke tuples (pragma lines can't carry `// want:` pins) rather than
/// `wants()`.
#[test]
fn bad_go_pragmas_each_yield_one_bad_annotation_finding() {
    let p = goverify_ir::testutil::load_corpus("annot");
    let ann = compile_program(&p, KNOWN);
    let bad: Vec<(String, u32)> = ann
        .findings
        .iter()
        .filter(|f| f.checker == "bad-annotation")
        .map(|f| {
            let pos = f.pos.as_ref().expect("bad-annotation carries pragma pos");
            (pos.file.clone(), pos.line)
        })
        .collect();
    assert_eq!(bad.len(), 8, "one per bad.go pragma: {bad:?}");
}

/// `Suppressed`'s `//goverify:ignore nil` compiles to exactly the
/// checker name `nil` in its `FuncAnnotations.ignores` — the metadata
/// the CLI's pragma-ignore filter consumes (phase-6 spec §5).
#[test]
fn suppressed_ignore_set_compiles_and_validates() {
    let p = goverify_ir::testutil::load_corpus("annot");
    let ann = compile_program(&p, KNOWN);
    let f = p.lookup_func("example.com/annot.Suppressed").unwrap();
    assert_eq!(ann.funcs[&f].ignores, vec!["nil".to_string()]);
}

/// The engine's raw findings (annotations active, ignores NOT applied —
/// that's CLI-only) over `example.com/annot`, excluding
/// `unverified-annotation` (Zero's warning is asserted separately below
/// since it can't carry a `// want:` pin — its finding anchors to a
/// whole-line pragma comment), must match the corpus's `// want:` pins
/// exactly: `CallsPositiveBad`'s contract violation, and both
/// `Reported`'s and `Suppressed`'s nil-deref plus `Suppressed`'s bounds
/// finding (the (func, checker) conjunction the ignore addendum pins).
#[test]
fn engine_findings_match_want_pins_with_annotations_active() {
    let (_p, _ann, a) = analyze();
    let got: BTreeSet<(String, u32, String)> = a
        .findings
        .iter()
        .filter(|f| f.func.contains("example.com/annot"))
        .filter(|f| f.checker != UNVERIFIED_ANNOTATION)
        .filter_map(|f| {
            f.pos
                .as_ref()
                .map(|p| (p.file.clone(), p.line, f.tag.clone()))
        })
        .collect();
    let want: BTreeSet<(String, u32, String)> =
        goverify_ir::testutil::wants("annot").into_iter().collect();
    assert_eq!(got, want, "findings vs want pins");
}

/// `Zero`'s ensures is false (returns 0, claims `ret >= 1`): must warn,
/// anchored at its own pragma line, Warning severity. `One`'s ensures is
/// true and provable: must stay silent.
#[test]
fn zero_ensures_warns_and_one_stays_verified() {
    let (_p, _ann, a) = analyze();
    let warns: Vec<&Finding> = a
        .findings
        .iter()
        .filter(|f| f.checker == UNVERIFIED_ANNOTATION)
        .collect();
    assert!(
        warns.iter().any(|f| f.func.ends_with("annot.Zero")),
        "Zero must warn: {warns:?}"
    );
    assert!(
        !warns.iter().any(|f| f.func.ends_with("annot.One")),
        "One's ensures is proven and must not warn: {warns:?}"
    );
    assert!(
        warns.iter().all(|f| f.severity == Severity::Warning),
        "unverified-annotation findings are always Warning severity: {warns:?}"
    );
}

/// `Positive`'s merged summary carries its annotated `requires` clause
/// (phase-6 spec §4's merge rule: annotated + inferred requires
/// coexist), tagged `contract` with `Provenance::Annotated` — the
/// summary-level evidence backing `CallsPositiveBad`'s finding.
#[test]
fn positive_summary_carries_annotated_contract_clause() {
    let (p, _ann, a) = analyze();
    let fp = p.lookup_func("example.com/annot.Positive").unwrap();
    assert!(
        a.summaries[&fp]
            .requires
            .iter()
            .any(|c| c.tag == CONTRACT && c.provenance == Provenance::Annotated),
        "Positive's summary must carry its annotated contract requires: {:?}",
        a.summaries[&fp].requires
    );
}

/// `Checked` exercises named-result resolution (extractor `result_names`,
/// declared names `ret`/`err`) through a real ensures pragma:
/// `err == nil ==> ret >= 0`. Contingency (brief, Step 2): whether the
/// engine actually PROVES this implication depends on solver/encoding
/// strength for the error-sentinel branch (`errAnnot`, a non-literal
/// error value) — this asserts nothing about provenness itself, only
/// that (a) exactly one ensures pragma compiled at all (the
/// named-result-resolution regression guard) and (b) IF it warns, the
/// warning sits at its own pragma line (never elsewhere).
#[test]
fn checked_compiles_one_ensures_and_any_warning_is_at_its_own_pragma() {
    let (p, ann, a) = analyze();
    let checked = p.lookup_func("example.com/annot.Checked").unwrap();
    assert_eq!(
        ann.funcs[&checked].ensures.len(),
        1,
        "Checked's single ensures pragma must compile to exactly one clause"
    );
    let pragma_pos = ann.funcs[&checked].ensures[0].pos.clone();
    for f in a
        .findings
        .iter()
        .filter(|f| f.checker == UNVERIFIED_ANNOTATION && f.func.ends_with("annot.Checked"))
    {
        assert_eq!(
            f.pos, pragma_pos,
            "Checked's unverified-annotation warning (if any) must be at its own pragma line"
        );
    }
}

/// `generic.go` pins the CURRENT (documented-limitation, fix-wave
/// item 1) behavior of a `//goverify:requires` pragma on a generic
/// function: go/ssa gives every instantiation of `GenPositive` (e.g.
/// `GenPositive[int]`, the interned name a static call site actually
/// targets) `Pkg == nil`, so the extractor never emits instantiations
/// as functions of their own — only the generic ORIGIN
/// (`example.com/annot.GenPositive`, no `[...]` suffix) is emitted, and
/// the pragma attaches to it via an exact decl_id match (`compile_program`
/// never needed to fan out to any instantiation, because none exist to
/// fan out to). This asserts three things: the pragma compiles cleanly
/// onto the origin (one requires clause, no bad-annotation finding — it
/// is a well-formed pragma on a real decl_id, just one the engine can't
/// yet act on at call sites), and `CallsGenPositiveBad`'s outright
/// violation (`GenPositive(0, 1)`, `n >= 1`) produces NO contract
/// finding, because the call's static callee is `GenPositive[int]`, an
/// annotation-free interned external. Flip this test's last assertion
/// (and add a `// want: contract` pin instead) once generic-origin
/// pragma fan-out lands (plan follow-up queue).
#[test]
fn generic_origin_pragma_attaches_but_call_site_is_unaffected() {
    let p = goverify_ir::testutil::load_corpus("annot");
    let ann = compile_program(&p, KNOWN);

    // The pragma compiles onto the origin's own decl_id — no `[...]`
    // suffix, since instantiations are never emitted as functions.
    let origin = p
        .lookup_func("example.com/annot.GenPositive")
        .expect("the generic origin must be interned under its unparameterized decl_id");
    assert_eq!(
        ann.funcs[&origin].requires.len(),
        1,
        "the pragma must compile onto the origin with exactly one requires clause: {:?}",
        ann.funcs.get(&origin)
    );
    assert!(
        p.lookup_func("example.com/annot.GenPositive[int]")
            .is_some(),
        "the instantiation IS interned (as a call-site callee) — just never as a function \
         with a body or its own pragma match"
    );
    assert!(
        ann.findings
            .iter()
            .all(|f| f.checker != "bad-annotation" || !f.func.contains("GenPositive")),
        "a well-formed pragma on a real decl_id must not raise bad-annotation: {:?}",
        ann.findings
    );

    // No contract finding anywhere for the outright-violating call:
    // the documented limitation, pinned so it flips loudly (this
    // assertion starts failing) once fan-out lands.
    let cfg = EngineConfig {
        opts: Options::default(),
        cache_dir: None,
        emit_smt: None,
        annotations: ann,
        annotation_version: ANNOTATION_VERSION,
    };
    let checkers = default_checkers();
    let a = analyze_full(&p, &cfg, &checkers, &|_role| {
        Box::new(Z3Native::new(limits()))
    });
    assert!(
        a.findings
            .iter()
            .all(|f| f.checker != CONTRACT || !f.func.ends_with("CallsGenPositiveBad")),
        "CURRENT documented limitation: a generic function's annotated requires does not \
         yet reach its call sites, so this outright violation produces no contract \
         finding: {:?}",
        a.findings
    );
}
