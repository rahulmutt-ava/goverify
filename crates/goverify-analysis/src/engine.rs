//! Bottom-up SCC engine (phase-2 spec §4.2–4.3): wave-parallel schedule,
//! bounded fixpoint on recursive SCCs, widening to havoc after k rounds,
//! catch_unwind per function. Phase-3 (Task 12) adds the `Checker` plugin
//! surface: requires-inference runs inside the fixpoint (per-SCC backend),
//! and a separate, deliberately SEQUENTIAL findings pass runs after every
//! wave has finished (determinism first — see the pass itself).
//!
//! Scheduling: **wave-parallel** — group SCCs by longest-path depth over
//! the condensation DAG (leaves = depth 0), process depths in ascending
//! order, `rayon` `par_iter` within a wave. A barrier per wave is mildly
//! pessimistic vs. true dataflow scheduling; chosen for simplicity, results
//! are deterministic either way because summaries are pure functions of
//! inputs. Revisit only if phase-5 profiling says so.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;

use goverify_cache::QueryCache;
use goverify_ir::{CallGraph, FuncId, Program, Sccs};
use goverify_solver::{Query, SatResult, StubSolver, TextSolver, discharge_query};

use crate::checker::{Checker, Finding};
use crate::effects::{self, Effects, Loc, Root};
use crate::prepass::{self, Domains};
use crate::summary::{Provenance, Summary};

/// Which of the two solver-timeout tiers a `mk_backend` call is for: the
/// fixpoint's per-SCC requires-inference backend runs many small queries
/// inline with analysis (tight timeout budget), while the findings pass's
/// backend discharges obligations that gate user-visible output (can
/// afford to wait longer for a definitive Sat/Unsat before giving up and
/// staying silent per the bug-finder policy). `debug findings` uses one
/// timeout for both roles for now; `check` (Task 11) differentiates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendRole {
    Infer,
    Findings,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub widen_after: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { widen_after: 3 }
    }
}

/// Everything `analyze_full` needs beyond the fixpoint options: an
/// optional on-disk query cache, and an optional directory to dump every
/// canonical SMT-LIB2 query to. Solver limits live with the backend the
/// `mk_backend` closure constructs — they are part of the backend's
/// identity (query-cache key), not engine state.
#[derive(Debug, Clone, Default)]
pub struct EngineConfig {
    pub opts: Options,
    pub cache_dir: Option<PathBuf>,
    pub emit_smt: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Analysis {
    pub summaries: BTreeMap<FuncId, Summary>,
    pub prepass: BTreeMap<FuncId, Domains>,
    pub diagnostics: Vec<String>,
    pub findings: Vec<Finding>,
}

/// Phase-2-compatible entry point: no checkers, no findings, `StubSolver`
/// (answers Unknown to everything) as the fixpoint's backend. Every
/// existing caller/test survives unmodified — this is a thin `analyze_full`
/// delegation, not a parallel implementation.
pub fn analyze(p: &Program, opts: &Options) -> Analysis {
    analyze_full(
        p,
        &EngineConfig {
            opts: opts.clone(),
            ..EngineConfig::default()
        },
        &[],
        &|_role| Box::new(StubSolver),
    )
}

/// The phase-3 tracer entry point (parent spec §8, §12): runs the same
/// wave-parallel fixpoint as `analyze`, additionally letting `checkers`
/// infer requires-clauses per function and, after every wave has settled,
/// raising and discharging obligations in one sequential findings pass.
///
/// `mk_backend` is called once per SCC task, exactly like `mk_solver` used
/// to be — each wave-worker gets its own backend instance, reused across
/// every function/round in that worker's SCC.
pub fn analyze_full(
    p: &Program,
    cfg: &EngineConfig,
    checkers: &[&dyn Checker],
    mk_backend: &(dyn Fn(BackendRole) -> Box<dyn TextSolver> + Sync),
) -> Analysis {
    let cache = cfg.cache_dir.clone().map(QueryCache::open);
    let emit_dir = cfg.emit_smt.clone();
    // Deterministic requires order (parent spec's determinism invariant):
    // the caller's checker order is not guaranteed sorted, so sort once by
    // name here rather than trusting call sites.
    let mut checkers: Vec<&dyn Checker> = checkers.to_vec();
    checkers.sort_by_key(|c| c.name());
    let checkers = &checkers[..];
    let graph = CallGraph::build(p);
    let sccs = Sccs::compute(p, &graph);
    let n_sccs = sccs.schedule().len();

    // Wave assignment: depth(scc) = 1 + max(depth of callee sccs). callee
    // sccs always precede `i` in schedule order (callees-first), so this
    // is a single forward pass.
    let mut depth = vec![0usize; n_sccs];
    for i in 0..n_sccs {
        depth[i] = sccs
            .callee_sccs(i)
            .iter()
            .map(|&d| depth[d] + 1)
            .max()
            .unwrap_or(0);
    }
    let max_depth = depth.iter().copied().max().unwrap_or(0);
    let mut waves: Vec<Vec<usize>> = vec![Vec::new(); max_depth + 1];
    for (i, &d) in depth.iter().enumerate() {
        waves[d].push(i); // schedule order within wave: deterministic
    }

    // Summaries live in a slot-per-function vec so waves can write in
    // parallel without locking the whole map. All callee slots for the
    // current wave were finalized by an earlier (already-joined) wave, so
    // reads never race writes.
    let n_funcs = p.func_ids().count();
    let slots: Vec<Mutex<Option<Summary>>> = (0..n_funcs).map(|_| Mutex::new(None)).collect();
    // Diagnostics get the same slot-per-function treatment as summaries
    // (rather than one shared `Mutex<Vec<String>>`): two functions in the
    // same wave can panic on different rayon worker threads, and which one
    // wins the mutex first is scheduling-dependent, not determined by any
    // sorted key. Storing at most one diagnostic per `FuncId` slot and
    // collecting them in a final single-threaded pass over `p.func_ids()`
    // (ascending, itself derived from sorted function names) makes the
    // resulting `Vec<String>` order independent of rayon's scheduling.
    let diag_slots: Vec<Mutex<Option<String>>> = (0..n_funcs).map(|_| Mutex::new(None)).collect();

    for wave in &waves {
        wave.par_iter().for_each(|&si| {
            let members = &sccs.schedule()[si];
            let recursive =
                members.len() > 1 || members.iter().any(|&m| graph.callees(m).contains(&m));
            let mut current: BTreeMap<FuncId, Summary> = members
                .iter()
                .map(|&m| (m, Summary::default())) // optimistic start
                .collect();
            let mut backend = mk_backend(BackendRole::Infer);
            let mut rounds = 0u32;
            // `analyze_function` re-runs every checker's `infer_requires`
            // on every round of a recursive SCC's fixpoint (it never reads
            // callee summaries, only `f`'s own body) — wasteful but
            // harmless: same function body ⇒ same clauses every time, so
            // the `current[&m] != new` convergence check below still
            // terminates exactly as it does for effects alone. Fine for
            // phase 3; revisit only if profiling says so.
            loop {
                let mut changed = false;
                for &m in members {
                    let new = analyze_function(
                        p,
                        &graph,
                        m,
                        &|f| read_slot(&slots, f, &current),
                        checkers,
                        &mut *backend,
                        cache.as_ref(),
                        emit_dir.as_deref(),
                        &diag_slots,
                    );
                    if current[&m] != new {
                        current.insert(m, new);
                        changed = true;
                    }
                }
                if !recursive || !changed {
                    break;
                }
                if rounds >= cfg.opts.widen_after {
                    // Widen: havoc every member. Widening only ever moves
                    // up the lattice (toward top), never invents
                    // constraints — `Summary::havoc()` has no requires.
                    for &m in members {
                        current.insert(m, Summary::havoc());
                    }
                    break;
                }
                rounds += 1;
            }
            for (m, s) in current {
                *slots[m.0 as usize].lock().unwrap() = Some(s);
            }
        });
    }

    // Assemble Analysis: summaries from slots (missing/external -> havoc),
    // prepass domains: value_clean via prepass::value_clean, concurrency_clean
    // = summary.effects.is_empty().
    let mut summaries = BTreeMap::new();
    let mut pre = BTreeMap::new();
    for f in p.func_ids() {
        let s = slots[f.0 as usize]
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(Summary::havoc);
        pre.insert(
            f,
            Domains {
                value_clean: prepass::value_clean(p, f),
                concurrency_clean: s.effects.is_empty(),
            },
        );
        summaries.insert(f, s);
    }
    // Single-threaded, ascending-FuncId pass: deterministic regardless of
    // which wave/thread produced each diagnostic.
    let mut diagnostics: Vec<String> = p
        .func_ids()
        .filter_map(|f| diag_slots[f.0 as usize].lock().unwrap().clone())
        .collect();

    // Findings pass (phase-3 spec §8, §12): SEQUENTIAL, not lazy. Every
    // summary is final at this point, so this is deliberately a second,
    // single-threaded scan over `p.func_ids()` (ascending order, itself
    // derived from sorted function names) with one fresh backend —
    // determinism first. Parallelize in phase 5 with the same slot
    // pattern as summaries above if profiling asks.
    let mut findings: Vec<Finding> = Vec::new();
    let mut findings_diagnostics: Vec<String> = Vec::new();
    if !checkers.is_empty() {
        let mut backend = mk_backend(BackendRole::Findings);
        let summary_of = |f: FuncId| summaries.get(&f).cloned().unwrap_or_else(Summary::havoc);
        for f in p.func_ids() {
            // A `Checker` is the phase-4 plugin surface: `obligations` +
            // `discharge_query` runs untrusted (to this engine) code, same
            // as `analyze_function`'s body above. Wrap it in the same
            // catch_unwind + diagnostic pattern so one panicking checker
            // can't kill the whole sequential findings pass — a panicking
            // function just contributes zero findings (degrade, never
            // die).
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut per_func: Vec<Finding> = Vec::new();
                for checker in checkers {
                    for ob in checker.obligations(p, f, &summary_of) {
                        // Bug-finder semantics (parent spec §8): only a
                        // confirmed Sat verdict becomes a Finding; Unsat and
                        // Unknown (incl. timeouts) stay silent.
                        let outcome = discharge_query(
                            &ob.query,
                            &mut *backend,
                            cache.as_ref(),
                            emit_dir.as_deref(),
                        );
                        if outcome.result == SatResult::Sat {
                            let trace = outcome
                                .model
                                .as_deref()
                                .and_then(|m| trace_for(p, f, m))
                                .unwrap_or_default();
                            per_func.push(Finding {
                                checker: checker.name().to_string(),
                                tag: ob.tag.clone(),
                                func: p.func_name(f).to_string(),
                                pos: ob.pos,
                                message: ob.message,
                                trace,
                            });
                        }
                    }
                }
                per_func
            }));
            match run {
                Ok(mut per_func) => {
                    per_func
                        .sort_by(|a, b| a.pos.cmp(&b.pos).then_with(|| a.message.cmp(&b.message)));
                    findings.extend(per_func);
                }
                Err(_) => {
                    findings_diagnostics.push(format!(
                        "internal: panic while checking {}; findings for this function dropped",
                        p.func_name(f)
                    ));
                }
            }
        }
    }
    diagnostics.extend(findings_diagnostics);

    Analysis {
        summaries,
        prepass: pre,
        diagnostics,
        findings,
    }
}

/// Reconstruct the violating path for a Sat finding: re-encode (cheap,
/// pure, deterministic), read guard values, walk. Any failure = no
/// trace (finding still reported).
fn trace_for(p: &Program, f: FuncId, model: &str) -> Option<Vec<crate::checker::TraceStep>> {
    let func = p.func(f)?;
    let enc = crate::encode::encode_func(p, f).ok()?;
    let guards = crate::encode::guard_values(model);
    let path = crate::encode::violating_path(func, &enc.dag_succs, &guards);
    if path.is_empty() {
        return None;
    }
    Some(
        path.into_iter()
            .map(|b| crate::checker::TraceStep {
                block: b,
                pos: func.blocks[b as usize]
                    .instrs
                    .iter()
                    .find_map(|i| i.pos.clone()),
            })
            .collect(),
    )
}

/// Read a callee's summary: prefer the in-flight SCC-local `current` map
/// (same-SCC callee, not yet committed to `slots`), falling back to the
/// already-finalized cross-SCC slot; a function with no summary anywhere
/// yet (shouldn't happen given wave ordering, but degrade rather than
/// panic) is havoc.
fn read_slot(
    slots: &[Mutex<Option<Summary>>],
    f: FuncId,
    current: &BTreeMap<FuncId, Summary>,
) -> Summary {
    if let Some(s) = current.get(&f) {
        return s.clone();
    }
    slots
        .get(f.0 as usize)
        .and_then(|slot| slot.lock().unwrap().clone())
        .unwrap_or_else(Summary::havoc)
}

#[allow(clippy::too_many_arguments)]
fn analyze_function(
    p: &Program,
    graph: &CallGraph,
    f: FuncId,
    summary_of: &dyn Fn(FuncId) -> Summary,
    checkers: &[&dyn Checker],
    backend: &mut dyn TextSolver,
    cache: Option<&QueryCache>,
    emit_dir: Option<&Path>,
    diag_slots: &[Mutex<Option<String>>],
) -> Summary {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if p.func(f).is_none() {
            return Summary::havoc(); // external / bodyless
        }
        let effects = effects::collect(p, f, graph, &|c| summary_of(c).effects);

        // Union of every checker's inferred requires: checkers run in
        // name-sorted order (analyze_full sorts once at entry — determinism
        // first), then per-checker clause order (Task 12 design). See the
        // recursive-SCC caveat above the calling loop.
        let mut requires = Vec::new();
        for checker in checkers {
            let mut discharge =
                |q: &Query| discharge_query(q, &mut *backend, cache, emit_dir).result;
            requires.extend(checker.infer_requires(p, f, summary_of, &mut discharge));
        }

        Summary {
            effects,
            requires,
            ..Summary::default()
        }
    }));
    match run {
        Ok(s) => s,
        Err(_) => {
            if let Some(slot) = diag_slots.get(f.0 as usize) {
                *slot.lock().unwrap() = Some(format!(
                    "internal: panic while analyzing {}; havoc summary substituted",
                    p.func_name(f)
                ));
            }
            Summary::havoc()
        }
    }
}

/// One line per matching function, sorted by function *name* (not
/// `FuncId`) for human diffing. `filter` is a substring match on the
/// function id.
pub fn dump_prepass(p: &Program, a: &Analysis, filter: Option<&str>) -> String {
    let mut lines: Vec<String> = a
        .prepass
        .iter()
        .map(|(&f, d)| (p.func_name(f), d))
        .filter(|(name, _)| filter.is_none_or(|s| name.contains(s)))
        .map(|(name, d)| {
            format!(
                "{name} value_clean={} concurrency_clean={}",
                d.value_clean, d.concurrency_clean
            )
        })
        .collect();
    lines.sort_unstable();
    render_lines(lines)
}

fn render_lines(lines: Vec<String>) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn render_loc(l: &Loc) -> String {
    let mut s = match &l.root {
        Root::Param(i) => format!("p{i}"),
        Root::Global(g) => format!("g:{g}"),
        Root::Alloc(v) => format!("alloc:{v}"),
        Root::Unknown => "?".to_string(),
    };
    for f in &l.path {
        s.push_str(&format!(".f{f}"));
    }
    s
}

fn render_loc_map<T: std::fmt::Debug>(m: &BTreeMap<Loc, BTreeSet<T>>) -> String {
    m.iter()
        .map(|(loc, ops)| {
            let ops: Vec<String> = ops.iter().map(|o| format!("{o:?}")).collect();
            format!("{}:[{}]", render_loc(loc), ops.join(","))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_effects(e: &Effects) -> String {
    format!(
        "{{spawns:{:?} chan:{{{}}} locks:{{{}}}}}",
        e.spawns,
        render_loc_map(&e.chan_ops),
        render_loc_map(&e.lock_ops)
    )
}

/// One line per matching function, sorted by function *name*. `filter` is
/// a substring match on the function id.
pub fn dump_summaries(p: &Program, a: &Analysis, filter: Option<&str>) -> String {
    let mut lines: Vec<String> = a
        .summaries
        .iter()
        .map(|(&f, s)| (p.func_name(f), s))
        .filter(|(name, _)| filter.is_none_or(|sub| name.contains(sub)))
        .map(|(name, s)| {
            let provenance = match s.provenance {
                Provenance::Inferred => "Inferred",
                Provenance::Havoc => "Havoc",
            };
            format!(
                "{name} effects={} requires={} ensures={} provenance={provenance}",
                render_effects(&s.effects),
                s.requires.len(),
                s.ensures.len(),
            )
        })
        .collect();
    lines.sort_unstable();
    render_lines(lines)
}

/// One line per finding, in already-sorted assembly order (per-function
/// groups sorted by `(pos, message)`, functions visited in
/// `p.func_ids()` order) — unlike `dump_prepass`/`dump_summaries`, this
/// does NOT re-sort by name: the assembly order IS the deterministic
/// order. `filter` is a substring match on `func` (same convention as
/// `dump_summaries`); whole-DAG extraction includes stdlib, so an
/// unfiltered corpus golden would flake on Go toolchain bumps.
pub fn dump_findings(a: &Analysis, filter: Option<&str>) -> String {
    let lines: Vec<String> = a
        .findings
        .iter()
        .filter(|f| filter.is_none_or(|s| f.func.contains(s)))
        .map(|f| {
            let pos = match &f.pos {
                Some(p) => format!("{}:{}:{}", p.file, p.line, p.col),
                None => "-:-:-".to_string(),
            };
            format!("{pos}: {}: {} [{}]", f.checker, f.message, f.func)
        })
        .collect();
    render_lines(lines)
}

#[cfg(test)]
mod tests {
    use goverify_ir::Program;

    use super::*;
    use crate::effects::{Effects, LockOp};
    use crate::summary::Provenance;
    use crate::testpkg::{block, call, func, instr, pkg};
    use goverify_solver::SolverLimits;

    fn straight(
        id: &str,
        body: Vec<goverify_extract::gvir::Instruction>,
    ) -> goverify_extract::gvir::Function {
        let mut instrs = body;
        instrs.push(instr("Return"));
        func(id, vec![block(0, instrs, vec![])])
    }

    #[test]
    fn effects_propagate_bottom_up() {
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                straight("t.Leaf", vec![call("(*sync.Mutex).Lock")]),
                straight("t.Mid", vec![call("t.Leaf")]),
                straight("t.Top", vec![call("t.Mid")]),
            ],
        )]);
        let a = analyze(&p, &Options::default());
        let top = p.lookup_func("t.Top").unwrap();
        let ops: BTreeSet<LockOp> = a.summaries[&top]
            .effects
            .lock_ops
            .values()
            .flatten()
            .copied()
            .collect();
        assert!(
            ops.contains(&LockOp::Lock),
            "Lock effect must propagate Leaf→Mid→Top"
        );
        assert!(!a.prepass[&top].concurrency_clean);
    }

    #[test]
    fn external_callee_gets_havoc_summary() {
        // unknown.G is interned via the call but has no body anywhere.
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![straight("t.F", vec![call("unknown.G")])],
        )]);
        let a = analyze(&p, &Options::default());
        let f = p.lookup_func("t.F").unwrap();
        assert_eq!(
            a.summaries[&f].effects,
            Effects::top(),
            "havoc callee effects must flow into the caller"
        );
        let g = p.lookup_func("unknown.G").unwrap();
        assert_eq!(a.summaries[&g].provenance, Provenance::Havoc);
    }

    #[test]
    fn recursive_scc_converges_without_widening() {
        // t.Even <-> t.Odd, no concurrency ops: fixpoint stabilizes at
        // empty effects immediately; provenance stays Inferred.
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                straight("t.Even", vec![call("t.Odd")]),
                straight("t.Odd", vec![call("t.Even")]),
            ],
        )]);
        let a = analyze(&p, &Options::default());
        let even = p.lookup_func("t.Even").unwrap();
        assert_eq!(a.summaries[&even].provenance, Provenance::Inferred);
        assert!(a.summaries[&even].effects.is_empty());
    }

    #[test]
    fn widening_kicks_in_after_k_rounds() {
        // The Lock op makes round 1 change the SCC's summaries (empty ->
        // {Lock}); with widen_after = 0 that first change triggers
        // widening, so the whole SCC comes out Havoc instead of iterating
        // to the (reachable) fixpoint.
        let evenodd = || {
            pkg(
                "t",
                vec![
                    straight("t.Even", vec![call("(*sync.Mutex).Lock"), call("t.Odd")]),
                    straight("t.Odd", vec![call("t.Even")]),
                ],
            )
        };
        let a0 = analyze(
            &Program::from_packages(vec![evenodd()]),
            &Options { widen_after: 0 },
        );
        let p = Program::from_packages(vec![evenodd()]);
        let even = p.lookup_func("t.Even").unwrap();
        assert_eq!(a0.summaries[&even].provenance, Provenance::Havoc);
        // Sanity: with the default k the same SCC converges Inferred.
        let a3 = analyze(&p, &Options::default());
        assert_eq!(a3.summaries[&even].provenance, Provenance::Inferred);
    }

    /// Scripted always-Sat backend: every obligation it discharges comes
    /// back Sat, so a fake checker's obligation must become a Finding.
    struct AlwaysSat;
    impl TextSolver for AlwaysSat {
        fn identity(&self) -> String {
            "always-sat".into()
        }
        fn limits(&self) -> SolverLimits {
            SolverLimits::default()
        }
        fn solve_text(&mut self, _canonical: &str) -> goverify_solver::QueryOutcome {
            goverify_solver::QueryOutcome {
                result: SatResult::Sat,
                model: None,
            }
        }
    }

    /// Infers nothing; raises exactly one obligation per function it is
    /// asked about.
    struct FakeChecker;
    impl Checker for FakeChecker {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn infer_requires(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
            _discharge: &mut dyn FnMut(&Query) -> SatResult,
        ) -> Vec<crate::summary::Clause> {
            Vec::new()
        }
        fn obligations(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
        ) -> Vec<crate::checker::Obligation> {
            vec![crate::checker::Obligation {
                tag: "fake".into(),
                message: "fake finding".into(),
                pos: None,
                query: Query::for_asserts(
                    goverify_solver::Logic::All,
                    vec![goverify_solver::Term::bool_lit(true)],
                ),
            }]
        }
    }

    /// Raises one obligation per function like `FakeChecker`, except it
    /// panics instead of returning obligations for one specific function
    /// name — the findings-pass analogue of `analyze_function`'s
    /// panicking-body regression coverage.
    struct PanicOnChecker(&'static str);
    impl Checker for PanicOnChecker {
        fn name(&self) -> &'static str {
            "panic-on"
        }
        fn infer_requires(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
            _discharge: &mut dyn FnMut(&Query) -> SatResult,
        ) -> Vec<crate::summary::Clause> {
            Vec::new()
        }
        fn obligations(
            &self,
            p: &Program,
            f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
        ) -> Vec<crate::checker::Obligation> {
            if p.func_name(f) == self.0 {
                panic!("PanicOnChecker: intentional panic analyzing {}", self.0);
            }
            vec![crate::checker::Obligation {
                tag: "panic-on".into(),
                message: "survives".into(),
                pos: None,
                query: Query::for_asserts(
                    goverify_solver::Logic::All,
                    vec![goverify_solver::Term::bool_lit(true)],
                ),
            }]
        }
    }

    #[test]
    fn findings_pass_panic_is_caught_and_other_functions_survive() {
        // Three independent (non-calling) functions; the checker panics
        // only while checking t.B. Degrade-never-die (parent spec's
        // panic policy) must hold for the findings pass exactly as it
        // does for `analyze_function`: t.A and t.C still get their
        // findings, the whole run completes rather than unwinding out of
        // `analyze_full`, and a diagnostic names the panicking function.
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                straight("t.A", vec![]),
                straight("t.B", vec![]),
                straight("t.C", vec![]),
            ],
        )]);
        let checkers: Vec<&dyn Checker> = vec![&PanicOnChecker("t.B")];
        let cfg = EngineConfig::default();
        let a = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(AlwaysSat));

        let found_funcs: BTreeSet<&str> = a.findings.iter().map(|f| f.func.as_str()).collect();
        assert!(
            found_funcs.contains("t.A"),
            "t.A's finding must survive t.B's panic: {:?}",
            a.findings
        );
        assert!(
            found_funcs.contains("t.C"),
            "t.C's finding must survive t.B's panic: {:?}",
            a.findings
        );
        assert!(
            !found_funcs.contains("t.B"),
            "the panicking function contributes zero findings: {:?}",
            a.findings
        );
        assert!(
            a.diagnostics.iter().any(|d| d.contains("t.B")),
            "a diagnostic must mention the panicking function: {:?}",
            a.diagnostics
        );
    }

    /// Same scripted always-Sat backend as `AlwaysSat`, except it carries a
    /// canned model string — the findings pass must read it and attach a
    /// trace, unlike `AlwaysSat`'s `model: None` (traceless findings).
    struct AlwaysSatWithModel;
    impl TextSolver for AlwaysSatWithModel {
        fn identity(&self) -> String {
            "always-sat-with-model".into()
        }
        fn limits(&self) -> SolverLimits {
            SolverLimits::default()
        }
        fn solve_text(&mut self, _canonical: &str) -> goverify_solver::QueryOutcome {
            goverify_solver::QueryOutcome {
                result: SatResult::Sat,
                model: Some("g0 -> true\n".into()),
            }
        }
    }

    #[test]
    fn sat_finding_with_model_gets_a_trace_but_model_none_stays_traceless() {
        let p = Program::from_packages(vec![pkg("t", vec![straight("t.F", vec![])])]);
        let checkers: Vec<&dyn Checker> = vec![&FakeChecker];
        let cfg = EngineConfig::default();

        let with_model = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(AlwaysSatWithModel));
        assert_eq!(with_model.findings.len(), 1);
        assert_eq!(
            with_model.findings[0].trace,
            vec![crate::checker::TraceStep {
                block: 0,
                pos: None
            }],
            "a Sat model with g0 -> true must produce a [block 0] trace: {:?}",
            with_model.findings
        );

        let without_model = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(AlwaysSat));
        assert_eq!(without_model.findings.len(), 1);
        assert!(
            without_model.findings[0].trace.is_empty(),
            "AlwaysSat (model: None) must keep producing traceless findings: {:?}",
            without_model.findings
        );
    }

    #[test]
    fn findings_pass_is_sequential_and_deterministic() {
        // Single-function program: obligations() fires exactly once, the
        // always-Sat backend confirms it, so exactly one Finding results
        // — and, since the findings pass is a single-threaded scan (no
        // rayon involved), it must come out byte-identical across runs.
        let p = Program::from_packages(vec![pkg("t", vec![straight("t.F", vec![])])]);
        let checkers: Vec<&dyn Checker> = vec![&FakeChecker];
        let cfg = EngineConfig::default();
        let a1 = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(AlwaysSat));
        let a2 = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(AlwaysSat));
        assert_eq!(
            a1.findings.len(),
            1,
            "one Sat obligation must yield one finding: {:?}",
            a1.findings
        );
        assert_eq!(a1.findings[0].checker, "fake");
        assert_eq!(
            a1.findings, a2.findings,
            "findings pass must be deterministic across runs"
        );
    }
}
