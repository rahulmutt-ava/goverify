//! SCC engine over the real corpus (phase-2 spec §4.2–4.3): determinism
//! despite rayon's wave-parallel scheduling, and sanity checks on the
//! effects/prepass facts inferred for a small real concurrency-heavy
//! package.

use goverify_analysis::{Options, Spawns, analyze, dump_summaries};

#[test]
fn analysis_is_deterministic_across_runs() {
    let p = goverify_ir::testutil::load_corpus("conc");
    let a1 = analyze(&p, &Options::default());
    let a2 = analyze(&p, &Options::default());
    assert_eq!(
        a1.summaries, a2.summaries,
        "rayon wave scheduling must not leak into summaries"
    );
    assert_eq!(
        a1.prepass, a2.prepass,
        "rayon wave scheduling must not leak into prepass domains"
    );
}

#[test]
fn conc_corpus_effects_are_sane() {
    let p = goverify_ir::testutil::load_corpus("conc");
    let a = analyze(&p, &Options::default());
    let close = p.lookup_func("(*example.com/conc.File).Close").unwrap();
    let e = &a.summaries[&close].effects;
    assert!(
        e.lock_ops.contains(&goverify_analysis::LockOp::Lock)
            && e.lock_ops.contains(&goverify_analysis::LockOp::Unlock),
        "Close locks and (deferred) unlocks: {e:?}"
    );
    let fan = p.lookup_func("example.com/conc.Fan").unwrap();
    assert_ne!(a.summaries[&fan].effects.spawns, Spawns::None);
    assert!(!a.prepass[&fan].concurrency_clean);
}

#[test]
fn conc_summaries_match_golden() {
    let p = goverify_ir::testutil::load_corpus("conc");
    let a = analyze(&p, &Options::default());
    // Only conc's own functions: stdlib summaries churn with Go bumps.
    let text = dump_summaries(&p, &a, Some("example.com/conc"));
    goverify_ir::testutil::check_golden("conc.summary.txt", &text);
}
