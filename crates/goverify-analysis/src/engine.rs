//! Bottom-up SCC engine (phase-2 spec §4.2–4.3): wave-parallel schedule,
//! bounded fixpoint on recursive SCCs, widening to havoc after k rounds,
//! catch_unwind per function.
//!
//! Scheduling: **wave-parallel** — group SCCs by longest-path depth over
//! the condensation DAG (leaves = depth 0), process depths in ascending
//! order, `rayon` `par_iter` within a wave. A barrier per wave is mildly
//! pessimistic vs. true dataflow scheduling; chosen for simplicity, results
//! are deterministic either way because summaries are pure functions of
//! inputs. Revisit only if phase-5 profiling says so.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use rayon::prelude::*;

use goverify_ir::{CallGraph, Callee, FuncId, Op, Program, Sccs};
use goverify_solver::{SatResult, Solver, StubSolver};

use crate::effects::{self, Effects, Loc, Root};
use crate::prepass::{self, Domains};
use crate::summary::{BoundClause, Provenance, Summary, instantiate_requires};

#[derive(Debug, Clone)]
pub struct Options {
    pub widen_after: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { widen_after: 3 }
    }
}

#[derive(Debug)]
pub struct Analysis {
    pub summaries: BTreeMap<FuncId, Summary>,
    pub prepass: BTreeMap<FuncId, Domains>,
    pub diagnostics: Vec<String>,
}

/// A reported violation. Phase 4 gives this real content; phase 2 only
/// needs it to exist so the discharge path is exercised end to end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub tag: String,
}

/// Discharge instantiated requires-clauses. Bug-finder semantics (parent
/// spec §8): only Sat reports; Unsat and Unknown (incl. timeout) are
/// silent. Unbindable clauses (violation: None) are silent by
/// construction.
pub fn discharge(obligations: &[BoundClause], solver: &mut dyn Solver) -> Vec<Finding> {
    obligations
        .iter()
        .filter_map(|o| {
            let v = o.violation.clone()?;
            solver.push();
            solver.assert(v);
            let r = solver.check_sat_assuming(&[]);
            solver.pop();
            (r == SatResult::Sat).then(|| Finding { tag: o.tag.clone() })
        })
        .collect()
}

pub fn analyze(p: &Program, opts: &Options) -> Analysis {
    analyze_with_solver(p, opts, &|| Box::new(StubSolver))
}

pub fn analyze_with_solver(
    p: &Program,
    opts: &Options,
    mk_solver: &(dyn Fn() -> Box<dyn Solver> + Sync),
) -> Analysis {
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
            let mut solver = mk_solver();
            let mut rounds = 0u32;
            loop {
                let mut changed = false;
                for &m in members {
                    let new = analyze_function(
                        p,
                        &graph,
                        m,
                        &|f| read_slot(&slots, f, &current),
                        &mut *solver,
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
                if rounds >= opts.widen_after {
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
    let diagnostics: Vec<String> = p
        .func_ids()
        .filter_map(|f| diag_slots[f.0 as usize].lock().unwrap().clone())
        .collect();
    Analysis {
        summaries,
        prepass: pre,
        diagnostics,
    }
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

fn analyze_function(
    p: &Program,
    graph: &CallGraph,
    f: FuncId,
    summary_of: &dyn Fn(FuncId) -> Summary,
    solver: &mut dyn Solver,
    diag_slots: &[Mutex<Option<String>>],
) -> Summary {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(func) = p.func(f) else {
            return Summary::havoc(); // external / bodyless
        };
        let effects = effects::collect(p, f, graph, &|c| summary_of(c).effects);

        // Thread the solver through the engine (spec §6): for every static
        // call site, instantiate the callee's requires and discharge them.
        // Phase 2 never populates `requires` on any summary, so this
        // always yields zero findings on real corpora — it exists so a
        // later phase swaps `StubSolver` for a real one at this call site
        // without touching the engine's structure.
        for b in &func.blocks {
            for ins in &b.instrs {
                if let Op::Call {
                    callee: Callee::Static(callee_id),
                    args,
                    ..
                } = &ins.op
                {
                    let callee_summary = summary_of(*callee_id);
                    let arg_terms: Vec<Option<goverify_solver::Term>> =
                        args.iter().map(|_| None).collect();
                    let obligations = instantiate_requires(&callee_summary, &arg_terms);
                    let _findings = discharge(&obligations, solver);
                }
            }
        }

        Summary {
            effects,
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

#[cfg(test)]
mod tests {
    use goverify_ir::Program;

    use super::*;
    use crate::effects::{Effects, LockOp};
    use crate::summary::Provenance;
    use crate::testpkg::{block, call, func, instr, pkg};

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

    #[test]
    fn discharge_with_stub_solver_reports_nothing() {
        let obligations = vec![BoundClause {
            tag: "nonnil".into(),
            violation: Some(goverify_solver::Term::bool_lit(true)),
        }];
        assert!(
            discharge(&obligations, &mut StubSolver).is_empty(),
            "Unknown must never produce a finding"
        );
    }
}
