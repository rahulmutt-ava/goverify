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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rayon::prelude::*;

use goverify_cache::QueryCache;
use goverify_ir::{CallGraph, FuncId, Program, Sccs};
use goverify_solver::{Query, SatResult, StubSolver, Term, TextSolver, discharge_query};

use crate::annotations::{Annotations, FuncAnnotations};
use crate::checker::{Checker, Finding};
use crate::effects::{self, Effects, Loc, Root};
use crate::prepass::{self, Domains};
use crate::summary::{self, Provenance, Summary};

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
    /// Compiled `//goverify:` annotations (phase-6 spec §3-§5). Default
    /// empty — every existing caller that builds `EngineConfig` without
    /// annotation support keeps behaving exactly as before.
    pub annotations: Annotations,
    /// `goverify_spec::ANNOTATION_VERSION` (phase-6 spec §4), salted into
    /// the SCC cache alongside `SCC_CACHE_VERSION`: a compiler-semantics
    /// change to how pragmas lower to clauses must rotate every cached
    /// entry even though the pragma TEXT (already in `func_ir_hash`) and
    /// the engine-pass code (covered by `SCC_CACHE_VERSION`) haven't
    /// changed. `goverify-analysis` deliberately does not depend on
    /// `goverify-spec` (crate boundary), so this is a bare `u32` the CLI
    /// populates from the real constant; default 0 keeps every existing
    /// caller/test byte-identical until Task 10 wires it up.
    pub annotation_version: u32,
}

#[derive(Debug)]
pub struct Analysis {
    pub summaries: BTreeMap<FuncId, Summary>,
    pub prepass: BTreeMap<FuncId, Domains>,
    pub diagnostics: Vec<String>,
    pub findings: Vec<Finding>,
    /// Per-SCC cache accounting (phase-5a spec §4). Both 0 when
    /// `cfg.cache_dir` is `None` or `checkers` is empty (no cache is
    /// constructed in either case).
    pub scc_cache_hits: u64,
    pub scc_cache_misses: u64,
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
    let annotations = &cfg.annotations;
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

    // Per-SCC cache (phase-5a spec §4). Only constructed when a cache dir
    // is configured AND there are checkers AND we are not dumping SMT: the
    // checker-less debug prepass/summary paths run the fixpoint with no
    // findings and must stay allocation-free (and never poison entries).
    // `emit_smt` is a debug/audit mode whose whole point is a COMPLETE dump
    // of every discharged query; an SCC cache hit replays a summary/finding
    // without ever re-entering discharge, so a warm run would silently emit
    // only the missed SCCs' queries. Disable the cache entirely under
    // emit_smt so the dump is always complete (correctness over speed here).
    // Probe one backend per role for identity/limits — with the lazy
    // escalated tier this allocates one Z3 context per probe, freed
    // immediately.
    let scc_cache = cfg
        .cache_dir
        .clone()
        .filter(|_| !checkers.is_empty())
        .filter(|_| cfg.emit_smt.is_none())
        .map(|root| {
            let infer_probe = mk_backend(BackendRole::Infer);
            let findings_probe = mk_backend(BackendRole::Findings);
            crate::scc_cache::SccCache::open(
                root,
                &crate::scc_cache::CacheConfigKey {
                    solver_identity: infer_probe.identity(),
                    findings_identity: findings_probe.identity(),
                    infer_limits: infer_probe.limits(),
                    findings_limits: findings_probe.limits(),
                    widen_after: cfg.opts.widen_after,
                    checkers: checkers.iter().map(|c| (c.name(), c.version())).collect(),
                    annotation_version: cfg.annotation_version,
                },
            )
        });
    let scc_keys = scc_cache.as_ref().map(|c| c.keys(p, &sccs));
    let hits = AtomicU64::new(0);
    let misses = AtomicU64::new(0);
    // Replay payload per function: findings + findings-phase diags from a
    // cache hit, consumed by the sequential findings pass below.
    struct Replay {
        findings: Vec<Finding>,
        findings_diags: Vec<String>,
    }
    let replay_slots: Vec<Mutex<Option<Replay>>> = (0..n_funcs).map(|_| Mutex::new(None)).collect();
    // Track which schedule positions were misses (need a `put` later).
    let fresh_sccs: Vec<AtomicBool> = (0..n_sccs).map(|_| AtomicBool::new(false)).collect();

    for wave in &waves {
        wave.par_iter().for_each(|&si| {
            let members = &sccs.schedule()[si];
            // Cache hit: replay this SCC's cached summaries/diags/findings
            // straight into the slots, skipping analysis entirely. The
            // integrity guard (`entry_matches`) turns any decoded entry
            // whose member names don't line up with this SCC into a miss.
            if let (Some(cache), Some(keys)) = (scc_cache.as_ref(), scc_keys.as_ref())
                && let Some(entry) = cache.get(&keys[si])
                && entry_matches(members, &entry, p)
            {
                hits.fetch_add(1, Ordering::Relaxed);
                for (m, me) in members.iter().zip(entry.members) {
                    *slots[m.0 as usize].lock().unwrap() = Some(me.summary);
                    *diag_slots[m.0 as usize].lock().unwrap() = me.analysis_diag;
                    *replay_slots[m.0 as usize].lock().unwrap() = Some(Replay {
                        findings: me.findings,
                        findings_diags: me.findings_diags,
                    });
                }
                return;
            }
            if scc_cache.is_some() {
                misses.fetch_add(1, Ordering::Relaxed);
                fresh_sccs[si].store(true, Ordering::Relaxed);
            }
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
                        &|f| read_slot(&slots, f, &current, annotations),
                        checkers,
                        &mut *backend,
                        cache.as_ref(),
                        emit_dir.as_deref(),
                        &diag_slots,
                        annotations.funcs.get(&m),
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
                    // inferred constraints — `Summary::havoc()` has no
                    // requires. Annotated clauses are constants, not
                    // fixpoint state (phase-6 spec §4): they survive
                    // widening via `havoc_with`.
                    for &m in members {
                        current.insert(m, summary::havoc_with(annotations.funcs.get(&m)));
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
        // Missing slot -> no summary was ever computed for `f` (fully
        // external, never scheduled into any SCC): havoc, plus whatever
        // annotations its own defining package attached (phase-6 spec
        // §4) — an external function's pragma still helps its callers.
        let s = slots[f.0 as usize]
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| summary::havoc_with(annotations.funcs.get(&f)));
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
    // Per-function fresh-path capture, consumed by the post-findings `put`
    // below. Only written meaningfully when the cache is on; a `None`-vec
    // is cheap otherwise.
    let mut fresh_out: Vec<Option<Vec<Finding>>> = vec![None; n_funcs];
    let mut fresh_diags: Vec<Vec<String>> = vec![Vec::new(); n_funcs];
    if !checkers.is_empty() {
        let mut backend = mk_backend(BackendRole::Findings);
        let summary_of = |f: FuncId| summaries.get(&f).cloned().unwrap_or_else(Summary::havoc);
        for f in p.func_ids() {
            // Cache-hit replay: this function's SCC was served from cache,
            // so replay its stored (pre-sorted) findings and findings-phase
            // diagnostics without encoding or solving anything.
            if let Some(r) = replay_slots[f.0 as usize].lock().unwrap().take() {
                findings.extend(r.findings);
                findings_diagnostics.extend(r.findings_diags);
                continue;
            }
            let diags_before = findings_diagnostics.len();
            // Spec §8 ("oversized function → skip with diagnostic"): every
            // checker encodes `f` internally and bails silently on an
            // `encode_func_with` error (e.g. a function past the assertion
            // cap), so no finding — and no reason why — ever surfaces.
            // Probe with the SAME `encode_func_with(.., &summary_of)` the
            // checkers use, not bare `encode_func`: a function under the cap
            // without summaries but over it once callee ensures are asserted
            // would otherwise get zero findings AND zero diagnostic,
            // defeating the one-diagnostic-per-failed-function contract.
            // Encode once here (findings pass only, so the fixpoint's cost
            // is untouched) and record ONE diagnostic per function when it
            // fails; the checkers still degrade to zero findings for it.
            // Bodyless/external functions never reach a checker's encoder,
            // so skip them. Deterministic: this loop is the single-threaded
            // ascending-`FuncId` findings scan.
            if p.func(f).is_some()
                && let Err(e) = crate::encode::encode_func_with(p, f, &summary_of)
            {
                findings_diagnostics.push(e);
            }
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
                            let model = outcome
                                .model
                                .as_deref()
                                .map(|m| {
                                    crate::encode::model_bindings(m)
                                        .into_iter()
                                        .filter(|(name, _)| is_param_binding(name))
                                        .collect()
                                })
                                .unwrap_or_default();
                            per_func.push(Finding {
                                checker: checker.name().to_string(),
                                tag: ob.tag.clone(),
                                func: p.func_name(f).to_string(),
                                pos: ob.pos,
                                message: ob.message,
                                trace,
                                model,
                                severity: crate::Severity::Error,
                            });
                        }
                    }
                }
                // Phase-6 annotation findings (spec §4-§5): call-site
                // obligations against callees' ANNOTATED requires, plus
                // best-effort verification of this function's own
                // annotated ensures. Deliberately inside this same
                // catch_unwind + per-function scan as the checker loop
                // above, so these findings enter `fresh_out` -> the SCC
                // cache entry -> warm replay exactly like any checker
                // finding. Only reachable when `!checkers.is_empty()`
                // (the whole findings pass is gated on that above) — a
                // no-checker run (`analyze`, debug paths) never promised
                // annotation findings either.
                let ann_of = |c: FuncId| annotations.funcs.get(&c).cloned();
                // Re-encode `f`: the probe above (for the encode-skip
                // diagnostic) discards its `Ok` value, and every
                // checker's `obligations`/`infer_*` already re-encodes
                // internally per call (see the loop above and
                // `analyze_function`'s per-round re-inference) — one
                // more encode here is the same wasteful-but-harmless
                // tradeoff, not a new one. `None` covers both "bodyless"
                // (`p.func(f)` itself is `None`) and "encode failed"
                // (oversized): contract obligations need call sites to
                // scan, so they only run in the `Some` case, while
                // `verify_ensures` handles the `None` case itself (warns
                // every annotated ensures rather than silently dropping
                // it).
                let enc_opt = p
                    .func(f)
                    .and_then(|_| crate::encode::encode_func_with(p, f, &summary_of).ok());
                // `f`'s own merged summary (annotated requires included):
                // needed by the contract pass (as `pre`, precision parity
                // with the checkers) AND by `verify_ensures` (assumed
                // alongside the body — requires holds at entry, design
                // spec §1(a) — so an ensures clause that only holds under
                // `f`'s own stated precondition is still provable, not
                // flagged unverified).
                let own = summary_of(f);
                if let (Some(func), Some(enc)) = (p.func(f), enc_opt.as_ref()) {
                    for ob in crate::annotations::contract_obligations(
                        p,
                        func,
                        enc,
                        &own,
                        &ann_of,
                        &summary_of,
                    ) {
                        // Same bug-finder semantics as the checker loop
                        // above: only a confirmed Sat verdict becomes a
                        // Finding.
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
                            let model = outcome
                                .model
                                .as_deref()
                                .map(|m| {
                                    crate::encode::model_bindings(m)
                                        .into_iter()
                                        .filter(|(name, _)| is_param_binding(name))
                                        .collect()
                                })
                                .unwrap_or_default();
                            per_func.push(Finding {
                                checker: crate::annotations::CONTRACT.to_string(),
                                tag: ob.tag.clone(),
                                func: p.func_name(f).to_string(),
                                pos: ob.pos,
                                message: ob.message,
                                trace,
                                model,
                                severity: crate::Severity::Error,
                            });
                        }
                    }
                }
                if let Some(fa) = annotations.funcs.get(&f) {
                    let own_requires: Vec<Term> = own
                        .requires
                        .iter()
                        .map(|c| c.formula.term.clone())
                        .collect();
                    let mut discharge = |q: &Query| {
                        discharge_query(q, &mut *backend, cache.as_ref(), emit_dir.as_deref())
                            .result
                    };
                    per_func.extend(crate::annotations::verify_ensures(
                        p,
                        f,
                        enc_opt.as_ref(),
                        fa,
                        &own_requires,
                        &mut discharge,
                    ));
                }
                per_func
            }));
            match run {
                Ok(mut per_func) => {
                    per_func
                        .sort_by(|a, b| a.pos.cmp(&b.pos).then_with(|| a.message.cmp(&b.message)));
                    if scc_cache.is_some() {
                        fresh_out[f.0 as usize] = Some(per_func.clone());
                    }
                    findings.extend(per_func);
                }
                Err(_) => {
                    findings_diagnostics.push(format!(
                        "internal: panic while checking {}; findings for this function dropped",
                        p.func_name(f)
                    ));
                    // A panicked function still caches its (empty findings +
                    // panic diagnostic) result: the empty findings are set
                    // explicitly here, the diagnostic is picked up by the
                    // `diags_before..` delta below.
                    if scc_cache.is_some() {
                        fresh_out[f.0 as usize] = Some(Vec::new());
                    }
                }
            }
            if scc_cache.is_some() {
                fresh_diags[f.0 as usize] = findings_diagnostics[diags_before..].to_vec();
            }
        }
    }
    diagnostics.extend(findings_diagnostics);

    // Post-findings put: persist every SCC that was freshly analyzed this
    // run (hits are already on disk). Members are written in schedule
    // order, matching what `entry_matches` expects on replay.
    if let (Some(cache), Some(keys)) = (scc_cache.as_ref(), scc_keys.as_ref()) {
        for si in 0..sccs.schedule().len() {
            if !fresh_sccs[si].load(Ordering::Relaxed) {
                continue;
            }
            let members = &sccs.schedule()[si];
            let entry = crate::scc_cache::SccEntry {
                members: members
                    .iter()
                    .map(|&m| crate::scc_cache::MemberEntry {
                        func: p.func_name(m).to_string(),
                        summary: slots[m.0 as usize]
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or_else(|| summary::havoc_with(annotations.funcs.get(&m))),
                        analysis_diag: diag_slots[m.0 as usize].lock().unwrap().clone(),
                        findings: fresh_out[m.0 as usize].clone().unwrap_or_default(),
                        findings_diags: fresh_diags[m.0 as usize].clone(),
                    })
                    .collect(),
            };
            // Write failure degrades to slower, never wrong (spec §5).
            let _ = cache.put(&keys[si], &entry);
        }
    }

    Analysis {
        summaries,
        prepass: pre,
        diagnostics,
        findings,
        scc_cache_hits: hits.load(Ordering::Relaxed),
        scc_cache_misses: misses.load(Ordering::Relaxed),
    }
}

/// True iff `name` is a `p<i>` encoder param name (same test as
/// `shared::params_only`, over a model-binding key rather than a
/// `Term`'s free vars): `Finding.model` only ever surfaces param
/// bindings, never internal `g<i>`/`v<i>` encoder temporaries.
fn is_param_binding(name: &str) -> bool {
    name.strip_prefix('p')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// Integrity guard for a decoded cache entry: its member list must line
/// up name-for-name with this SCC's schedule-order members. A hash
/// collision or stale-format entry that decodes but doesn't match is
/// treated as a miss (re-analyzed and overwritten), never a mismatch.
fn entry_matches(members: &[FuncId], entry: &crate::scc_cache::SccEntry, p: &Program) -> bool {
    members.len() == entry.members.len()
        && members
            .iter()
            .zip(&entry.members)
            .all(|(m, me)| p.func_name(*m) == me.func)
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
/// panic) is havoc plus whatever that function's own annotations state
/// (phase-6 spec §4: annotated clauses must survive every havoc
/// fallback, never just the widening one, so no un-merged summary can
/// ever reach a caller).
fn read_slot(
    slots: &[Mutex<Option<Summary>>],
    f: FuncId,
    current: &BTreeMap<FuncId, Summary>,
    annotations: &Annotations,
) -> Summary {
    if let Some(s) = current.get(&f) {
        return s.clone();
    }
    slots
        .get(f.0 as usize)
        .and_then(|slot| slot.lock().unwrap().clone())
        .unwrap_or_else(|| summary::havoc_with(annotations.funcs.get(&f)))
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
    ann: Option<&FuncAnnotations>,
) -> Summary {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if p.func(f).is_none() {
            // External / bodyless: no body to infer from, but a pragma
            // attached to the declaration (in its defining package)
            // still contributes a contract (phase-6 spec §4).
            return summary::havoc_with(ann);
        }
        let effects = effects::collect(p, f, graph, &|c| summary_of(c).effects);

        // Union of every checker's inferred requires: checkers run in
        // name-sorted order (analyze_full sorts once at entry — determinism
        // first), then per-checker clause order (Task 12 design). See the
        // recursive-SCC caveat above the calling loop.
        let mut requires = Vec::new();
        let mut ensures = Vec::new();
        for checker in checkers {
            let mut discharge =
                |q: &Query| discharge_query(q, &mut *backend, cache, emit_dir).result;
            requires.extend(checker.infer_requires(p, f, summary_of, &mut discharge));
            ensures.extend(checker.infer_ensures(p, f, summary_of, &mut discharge));
        }

        let mut s = Summary {
            effects,
            requires,
            ensures,
            ..Summary::default()
        };
        // Merge in this function's own annotated clauses (phase-6 spec
        // §4): dedup against what the checkers just inferred so a
        // contract they already prove doesn't double-report at call
        // sites.
        summary::merge_annotations(&mut s, ann);
        s
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
            summary::havoc_with(ann)
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
        Root::FreeVar(i) => format!("fv:{i}"),
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
                // Summary.provenance is never Annotated in practice (that
                // value is per-Clause only), but a corrupted/crafted SCC
                // cache entry could decode one — degrade to a label
                // rather than panic.
                Provenance::Annotated => "Annotated",
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
    use goverify_ir::{Pos, Program};

    use super::*;
    use crate::annotations::{AnnClause, CONTRACT, UNVERIFIED_ANNOTATION};
    use crate::checker::Severity;
    use crate::effects::{Effects, LockOp};
    use crate::summary::Provenance;
    use crate::testpkg::{block, call, func, func_with_params, instr, pkg};
    use goverify_solver::{SolverLimits, Sort, Term, Z3Native, ptr_is_nil, ptr_sort};

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

    /// Emits one ensures clause for every function: engine-plumbing probe.
    struct EnsuresChecker;
    impl Checker for EnsuresChecker {
        fn name(&self) -> &'static str {
            "ensures-probe"
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
        fn infer_ensures(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
            _discharge: &mut dyn FnMut(&Query) -> SatResult,
        ) -> Vec<crate::summary::Clause> {
            use goverify_solver::{Term, ptr_is_nil, ptr_sort};
            let r0 = Term::var("r0", ptr_sort());
            vec![crate::summary::Clause {
                tag: "nil-deref".into(),
                formula: crate::summary::Formula {
                    term: Term::not(ptr_is_nil(r0).unwrap()).unwrap(),
                },
                provenance: crate::summary::Provenance::Inferred,
            }]
        }
        fn obligations(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
        ) -> Vec<crate::checker::Obligation> {
            Vec::new()
        }
    }

    #[test]
    fn checker_ensures_land_in_summaries() {
        let p = Program::from_packages(vec![pkg("t", vec![straight("t.F", vec![])])]);
        let checkers: Vec<&dyn Checker> = vec![&EnsuresChecker];
        let a = analyze_full(&p, &EngineConfig::default(), &checkers, &|_role| {
            Box::new(StubSolver)
        });
        let f = p.lookup_func("t.F").unwrap();
        assert_eq!(
            a.summaries[&f].ensures.len(),
            1,
            "infer_ensures output must reach Summary.ensures: {:?}",
            a.summaries[&f]
        );
        assert_eq!(
            a.summaries[&f].ensures[0].tag, "nil-deref",
            "ensures clause tag must survive the engine collection: {:?}",
            a.summaries[&f]
        );
    }

    #[test]
    fn widening_drops_ensures() {
        // Recursive SCC + widen_after 0: the widened summary is havoc,
        // whose ensures are empty — an ensures clause must never survive
        // widening (soundness: empty is the weakest postcondition).
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                straight("t.Even", vec![call("(*sync.Mutex).Lock"), call("t.Odd")]),
                straight("t.Odd", vec![call("t.Even")]),
            ],
        )]);
        let checkers: Vec<&dyn Checker> = vec![&EnsuresChecker];
        let cfg = EngineConfig {
            opts: Options { widen_after: 0 },
            ..EngineConfig::default()
        };
        let a = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(StubSolver));
        let even = p.lookup_func("t.Even").unwrap();
        assert_eq!(
            a.summaries[&even].provenance,
            Provenance::Havoc,
            "widen_after 0 must widen this recursive SCC to Havoc: {:?}",
            a.summaries[&even]
        );
        assert!(
            a.summaries[&even].ensures.is_empty(),
            "widening must drop ensures: {:?}",
            a.summaries[&even]
        );
    }

    fn named_param(id: u32, name: &str) -> goverify_extract::gvir::Param {
        goverify_extract::gvir::Param {
            id,
            name: name.into(),
            r#type: 0,
        }
    }

    /// Infers exactly one requires clause (`¬is_nil(p0)`, tag
    /// "nil-deref") for every function it is asked about: the
    /// checker-side of the merge/dedup fixture.
    struct RequiresChecker;
    impl Checker for RequiresChecker {
        fn name(&self) -> &'static str {
            "requires-probe"
        }
        fn infer_requires(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
            _discharge: &mut dyn FnMut(&Query) -> SatResult,
        ) -> Vec<crate::summary::Clause> {
            let p0 = Term::var("p0", ptr_sort());
            vec![crate::summary::Clause {
                tag: "nil-deref".into(),
                formula: crate::summary::Formula {
                    term: Term::not(ptr_is_nil(p0).unwrap()).unwrap(),
                },
                provenance: Provenance::Inferred,
            }]
        }
        fn obligations(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
        ) -> Vec<crate::checker::Obligation> {
            Vec::new()
        }
    }

    #[test]
    fn merge_dedups_duplicate_annotated_requires_keeps_distinct_one() {
        // t.F has no body-derived facts of its own beyond RequiresChecker's
        // inferred ¬is_nil(p0). Two annotated requires are attached: one
        // whose formula duplicates that inferred clause exactly (dedup
        // rule, phase-6 spec §4 — must collapse to the single inferred
        // clause, original tag/provenance intact) and one with a distinct
        // formula (must survive as tag "contract", provenance Annotated).
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![func_with_params(
                "t.F",
                vec![named_param(1, "x")],
                vec![block(0, vec![instr("Return")], vec![])],
            )],
        )]);
        let f = p.lookup_func("t.F").unwrap();

        let p0 = Term::var("p0", ptr_sort());
        let dup_ann = AnnClause {
            clause: crate::summary::Clause {
                tag: CONTRACT.into(),
                formula: crate::summary::Formula {
                    term: Term::not(ptr_is_nil(p0.clone()).unwrap()).unwrap(),
                },
                provenance: Provenance::Annotated,
            },
            text: "x != nil".into(),
            pos: None,
        };
        // Deliberately the dup formula's negation: just needs to be a
        // DIFFERENT formula from the checker's inferred clause so the
        // dedup rule (formula equality) treats it as new.
        let new_ann = AnnClause {
            clause: crate::summary::Clause {
                tag: CONTRACT.into(),
                formula: crate::summary::Formula {
                    term: ptr_is_nil(p0).unwrap(),
                },
                provenance: Provenance::Annotated,
            },
            text: "test-only distinguishing formula".into(),
            pos: None,
        };
        let mut funcs = BTreeMap::new();
        funcs.insert(
            f,
            FuncAnnotations {
                requires: vec![dup_ann.clone(), new_ann.clone()],
                ensures: Vec::new(),
                ignores: Vec::new(),
            },
        );
        let cfg = EngineConfig {
            annotations: Annotations {
                funcs,
                findings: Vec::new(),
            },
            ..EngineConfig::default()
        };
        let checkers: Vec<&dyn Checker> = vec![&RequiresChecker];
        let a = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(StubSolver));
        let reqs = &a.summaries[&f].requires;
        assert_eq!(
            reqs.len(),
            2,
            "duplicate annotated clause must collapse into the inferred one, \
             distinct one must be added: {reqs:?}"
        );
        let dup_survivor = reqs
            .iter()
            .find(|c| c.formula == dup_ann.clause.formula)
            .expect("the duplicated formula must still be present exactly once");
        assert_eq!(
            dup_survivor.tag, "nil-deref",
            "the surviving clause must be the ORIGINAL inferred one, not the annotated \
             duplicate: {dup_survivor:?}"
        );
        assert_eq!(
            dup_survivor.provenance,
            Provenance::Inferred,
            "dedup must keep the inferred clause's own provenance: {dup_survivor:?}"
        );
        let new_survivor = reqs
            .iter()
            .find(|c| c.formula == new_ann.clause.formula)
            .expect("the non-duplicate annotated clause must be present");
        assert_eq!(
            new_survivor.tag, CONTRACT,
            "a non-duplicate annotated clause keeps the contract tag: {new_survivor:?}"
        );
        assert_eq!(
            new_survivor.provenance,
            Provenance::Annotated,
            "a non-duplicate annotated clause keeps Annotated provenance: {new_survivor:?}"
        );
    }

    #[test]
    fn widening_preserves_annotated_requires() {
        // Same recursive-SCC + widen_after:0 fixture as
        // `widening_drops_ensures`, but t.Even carries an annotated
        // requires. Widening discards fixpoint state, never human facts
        // (phase-6 spec §4): the widened (havoc-shaped) summary must
        // still carry exactly the annotated clause.
        let p = Program::from_packages(vec![pkg(
            "t",
            vec![
                straight("t.Even", vec![call("(*sync.Mutex).Lock"), call("t.Odd")]),
                straight("t.Odd", vec![call("t.Even")]),
            ],
        )]);
        let even = p.lookup_func("t.Even").unwrap();

        let p0 = Term::var("p0", ptr_sort());
        let ann_clause = crate::summary::Clause {
            tag: CONTRACT.into(),
            formula: crate::summary::Formula {
                term: Term::not(ptr_is_nil(p0).unwrap()).unwrap(),
            },
            provenance: Provenance::Annotated,
        };
        let mut funcs = BTreeMap::new();
        funcs.insert(
            even,
            FuncAnnotations {
                requires: vec![AnnClause {
                    clause: ann_clause.clone(),
                    text: "p != nil".into(),
                    pos: None,
                }],
                ensures: Vec::new(),
                ignores: Vec::new(),
            },
        );
        let cfg = EngineConfig {
            opts: Options { widen_after: 0 },
            annotations: Annotations {
                funcs,
                findings: Vec::new(),
            },
            ..EngineConfig::default()
        };
        let no_checkers: &[&dyn Checker] = &[];
        let a = analyze_full(&p, &cfg, no_checkers, &|_role| Box::new(StubSolver));
        assert_eq!(
            a.summaries[&even].provenance,
            Provenance::Havoc,
            "widen_after 0 must widen this recursive SCC to Havoc: {:?}",
            a.summaries[&even]
        );
        assert_eq!(
            a.summaries[&even].effects,
            Effects::top(),
            "widened summary must have top (havoc-shaped) effects: {:?}",
            a.summaries[&even]
        );
        assert_eq!(
            a.summaries[&even].requires,
            vec![ann_clause],
            "widening must preserve the annotated requires exactly: {:?}",
            a.summaries[&even]
        );
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

    /// Same shape as `AlwaysSatWithModel`, plus a param binding and a
    /// non-param `v<i>` encoder temporary in the model text — exercises
    /// Task 11's `Finding.model` population (filtered to `p<i>` names).
    struct AlwaysSatWithParamModel;
    impl TextSolver for AlwaysSatWithParamModel {
        fn identity(&self) -> String {
            "always-sat-with-param-model".into()
        }
        fn limits(&self) -> SolverLimits {
            SolverLimits::default()
        }
        fn solve_text(&mut self, _canonical: &str) -> goverify_solver::QueryOutcome {
            goverify_solver::QueryOutcome {
                result: SatResult::Sat,
                model: Some("g0 -> true\np0 -> (ptr-nil)\nv3 -> 7\n".into()),
            }
        }
    }

    #[test]
    fn finding_model_keeps_only_param_bindings() {
        let p = Program::from_packages(vec![pkg("t", vec![straight("t.F", vec![])])]);
        let checkers: Vec<&dyn Checker> = vec![&FakeChecker];
        let cfg = EngineConfig::default();
        let a = analyze_full(&p, &cfg, &checkers, &|_role| {
            Box::new(AlwaysSatWithParamModel)
        });
        assert_eq!(a.findings.len(), 1);
        assert_eq!(
            a.findings[0].model,
            vec![("p0".to_string(), "(ptr-nil)".to_string())],
            "g0/v3 are encoder plumbing, not param bindings: {:?}",
            a.findings
        );
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

    /// Mirrors the real checkers' `let Ok(enc) = encode_func(..) else {
    /// return Vec::new() }` pattern: silently contributes nothing for a
    /// function that fails to encode, and one trivially-Sat obligation for
    /// every function that DOES encode. Lets the F4 test show that an
    /// un-encodable function produces no finding while the engine still
    /// records the skip diagnostic.
    struct EncodeGatedChecker;
    impl Checker for EncodeGatedChecker {
        fn name(&self) -> &'static str {
            "encode-gated"
        }
        fn infer_requires(
            &self,
            p: &Program,
            f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
            _discharge: &mut dyn FnMut(&Query) -> SatResult,
        ) -> Vec<crate::summary::Clause> {
            // Same silent bail as the shipped checkers.
            let _ = crate::encode::encode_func(p, f);
            Vec::new()
        }
        fn obligations(
            &self,
            p: &Program,
            f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
        ) -> Vec<crate::checker::Obligation> {
            let Ok(_enc) = crate::encode::encode_func(p, f) else {
                return Vec::new();
            };
            vec![crate::checker::Obligation {
                tag: "encode-gated".into(),
                message: "encodable".into(),
                pos: None,
                query: Query::for_asserts(
                    goverify_solver::Logic::All,
                    vec![goverify_solver::Term::bool_lit(true)],
                ),
            }]
        }
    }

    /// A single-block function whose `int` BinOp count pushes the encoding
    /// past `encode::ASSERT_CAP` (each modeled BinOp adds one defining
    /// equality), so `encode_func` returns Err — the cheapest deterministic
    /// encode failure (the only other Err path is a bodyless function,
    /// which checkers skip before ever encoding).
    fn oversized_func(id: &str) -> goverify_extract::gvir::Function {
        use goverify_extract::gvir;
        // 50_000 BinOps -> 50_000 defining-equality asserts + the g0 guard
        // assert = 50_001 > ASSERT_CAP (50_000).
        const N: u32 = 50_000;
        let mut instrs: Vec<gvir::Instruction> = Vec::with_capacity(N as usize + 1);
        for r in 2..2 + N {
            instrs.push(gvir::Instruction {
                kind: "BinOp".into(),
                register: r,
                r#type: 1, // int
                operands: vec![1, 1],
                sem: Some(gvir::instruction::Sem::Binop(gvir::BinOpSem {
                    op: "+".into(),
                })),
                ..Default::default()
            });
        }
        instrs.push(gvir::Instruction {
            kind: "Return".into(),
            ..Default::default()
        });
        gvir::Function {
            id: id.into(),
            params: vec![gvir::Param {
                id: 1,
                name: "a".into(),
                r#type: 1,
            }],
            blocks: vec![gvir::BasicBlock {
                index: 0,
                instrs,
                succs: vec![],
                preds: vec![],
            }],
            ..Default::default()
        }
    }

    fn program_with_int_type(funcs: Vec<goverify_extract::gvir::Function>) -> Program {
        use goverify_extract::gvir;
        Program::from_packages(vec![gvir::Package {
            import_path: "t".into(),
            types: vec![gvir::Type {
                id: 1,
                repr: "int".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "int".into(),
                ..Default::default()
            }],
            functions: funcs,
            ..Default::default()
        }])
    }

    #[test]
    fn encode_failure_surfaces_one_engine_diagnostic_and_no_finding() {
        // t.Small encodes fine; t.Big blows past the assertion cap. Spec
        // §8 ("oversized function → skip with diagnostic"): the engine must
        // emit exactly ONE diagnostic naming t.Big and produce no finding
        // for it (the checker degrades to zero obligations), while t.Small
        // still reports normally.
        let p = program_with_int_type(vec![straight("t.Small", vec![]), oversized_func("t.Big")]);
        let checkers: Vec<&dyn Checker> = vec![&EncodeGatedChecker];
        let cfg = EngineConfig::default();
        let a = analyze_full(&p, &cfg, &checkers, &|_role| Box::new(AlwaysSat));

        let big_diags: Vec<&String> = a
            .diagnostics
            .iter()
            .filter(|d| d.contains("t.Big"))
            .collect();
        assert_eq!(
            big_diags.len(),
            1,
            "exactly one encode-skip diagnostic for the oversized function: {:?}",
            a.diagnostics
        );
        assert!(
            big_diags[0].contains("assertions"),
            "diagnostic must explain the assertion-cap skip: {:?}",
            big_diags
        );

        let found: BTreeSet<&str> = a.findings.iter().map(|f| f.func.as_str()).collect();
        assert!(
            !found.contains("t.Big"),
            "the un-encodable function must yield no finding: {:?}",
            a.findings
        );
        assert!(
            found.contains("t.Small"),
            "an encodable function still reports: {:?}",
            a.findings
        );
    }

    // ---- Task 8: contract call-site obligations, ensures verification,
    // cache salt ----

    /// Infers/raises nothing: the minimal non-empty checker set needed to
    /// exercise the annotation findings pass, which (like every finding)
    /// only runs when `!checkers.is_empty()` — see the comment at the
    /// integration site in `analyze_full`.
    struct NoOpChecker;
    impl Checker for NoOpChecker {
        fn name(&self) -> &'static str {
            "noop"
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
            Vec::new()
        }
    }

    fn z3_backend(_role: BackendRole) -> Box<dyn TextSolver> {
        Box::new(Z3Native::new(SolverLimits {
            timeout_ms: 5_000,
            mem_mb: 1024,
        }))
    }

    /// `p0 != 0` (BitVec-64), tag "contract"/Annotated: the shared
    /// requires clause both contract-obligation tests attach to
    /// `t.Callee`, `text` distinguishing the two only in the pos.
    fn nonzero_ann_clause(text: &str) -> AnnClause {
        let p0 = Term::var("p0", Sort::BitVec(64));
        AnnClause {
            clause: crate::summary::Clause {
                tag: CONTRACT.into(),
                formula: crate::summary::Formula {
                    term: Term::not(Term::eq(p0, Term::bv_lit(64, 0)).unwrap()).unwrap(),
                },
                provenance: Provenance::Annotated,
            },
            text: text.into(),
            pos: None,
        }
    }

    fn int_aux(id: u32, val: i64) -> goverify_extract::gvir::AuxValue {
        goverify_extract::gvir::AuxValue {
            id,
            kind: "Const".into(),
            r#type: 1, // int
            r#const: Some(goverify_extract::gvir::ConstValue {
                value: Some(goverify_extract::gvir::const_value::Value::Int(val)),
            }),
            ..Default::default()
        }
    }

    /// `t.Caller` calling `t.Callee(<arg_reg>)`, `arg_reg` naming a value
    /// already declared (aux constant or param) in the caller.
    fn caller_calling_callee(arg_reg: u32) -> goverify_extract::gvir::Function {
        goverify_extract::gvir::Function {
            id: "t.Caller".into(),
            blocks: vec![block(
                0,
                vec![
                    goverify_extract::gvir::Instruction {
                        kind: "Call".into(),
                        operands: vec![0, arg_reg], // [callee slot (unused), arg]
                        sem: Some(goverify_extract::gvir::instruction::Sem::Call(
                            goverify_extract::gvir::CallSem {
                                static_callee: "t.Callee".into(),
                                ..Default::default()
                            },
                        )),
                        ..Default::default()
                    },
                    instr("Return"),
                ],
                vec![],
            )],
            ..Default::default()
        }
    }

    #[test]
    fn contract_obligation_fires_on_violating_call_site_constant() {
        // t.Callee has an annotated requires `p0 != 0` (never given a
        // body — an external/bodyless callee's pragma still binds its
        // callers, phase-6 spec §4). t.Caller passes the literal
        // constant 0, which violates it outright — no caller-side
        // requires to contradict the violation, so the obligation must
        // be Sat and produce exactly one Error finding.
        let mut caller = caller_calling_callee(1);
        caller.aux = vec![int_aux(1, 0)];
        let p = program_with_int_type(vec![caller]);
        let callee = p.lookup_func("t.Callee").unwrap();

        let mut funcs = BTreeMap::new();
        funcs.insert(
            callee,
            FuncAnnotations {
                requires: vec![nonzero_ann_clause("p != 0")],
                ensures: Vec::new(),
                ignores: Vec::new(),
            },
        );
        let cfg = EngineConfig {
            annotations: Annotations {
                funcs,
                findings: Vec::new(),
            },
            ..EngineConfig::default()
        };
        let checkers: Vec<&dyn Checker> = vec![&NoOpChecker];
        let a = analyze_full(&p, &cfg, &checkers, &z3_backend);

        let contract_findings: Vec<&Finding> = a
            .findings
            .iter()
            .filter(|f| f.checker == CONTRACT)
            .collect();
        assert_eq!(
            contract_findings.len(),
            1,
            "a violating call-site constant must yield exactly one contract finding: {:?}",
            a.findings
        );
        assert_eq!(contract_findings[0].severity, Severity::Error);
        assert!(
            contract_findings[0].message.contains("p != 0"),
            "message must quote the annotation text: {:?}",
            contract_findings[0]
        );
    }

    #[test]
    fn contract_obligation_silent_when_caller_already_requires_it() {
        // Same t.Callee contract as above, but t.Caller forwards its OWN
        // param (itself annotated `p0 != 0`) instead of a bare constant.
        // Precision parity with the checkers (phase-6 spec §4): the
        // caller's own requires terms are asserted as `pre`, so the
        // instantiated violation directly contradicts them — Unsat,
        // silent.
        let mut caller = caller_calling_callee(1);
        caller.params = vec![goverify_extract::gvir::Param {
            id: 1,
            name: "x".into(),
            r#type: 1,
        }];
        let p = program_with_int_type(vec![caller]);
        let caller_id = p.lookup_func("t.Caller").unwrap();
        let callee = p.lookup_func("t.Callee").unwrap();

        let mut funcs = BTreeMap::new();
        funcs.insert(
            callee,
            FuncAnnotations {
                requires: vec![nonzero_ann_clause("p != 0")],
                ensures: Vec::new(),
                ignores: Vec::new(),
            },
        );
        funcs.insert(
            caller_id,
            FuncAnnotations {
                requires: vec![nonzero_ann_clause("x != 0")],
                ensures: Vec::new(),
                ignores: Vec::new(),
            },
        );
        let cfg = EngineConfig {
            annotations: Annotations {
                funcs,
                findings: Vec::new(),
            },
            ..EngineConfig::default()
        };
        let checkers: Vec<&dyn Checker> = vec![&NoOpChecker];
        let a = analyze_full(&p, &cfg, &checkers, &z3_backend);

        let contract_findings: Vec<&Finding> = a
            .findings
            .iter()
            .filter(|f| f.checker == CONTRACT)
            .collect();
        assert!(
            contract_findings.is_empty(),
            "a caller that already requires the callee's precondition must get no \
             contract finding: {:?}",
            a.findings
        );
    }

    /// Infers the SAME requires formula `nonzero_ann_clause` states
    /// (standing in for a real checker, e.g. `nil`, inferring a body-
    /// derived fact that happens to match an annotation verbatim), and
    /// raises exactly one obligation for `t.Caller` unconditionally
    /// (standing in for what that checker's own call-site-obligations
    /// pass — `shared::call_site_obligations` — would produce for a
    /// caller that violates the inferred clause). Both halves are
    /// deliberately simplified (no real dominance/reachability logic):
    /// this checker exists only to reproduce the fix-wave item 4 shape —
    /// an annotated requires whose formula duplicates a checker-inferred
    /// one — at the engine's findings-pass layer.
    struct DupChecker;
    impl Checker for DupChecker {
        fn name(&self) -> &'static str {
            "dupchecker"
        }
        fn infer_requires(
            &self,
            _p: &Program,
            _f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
            _discharge: &mut dyn FnMut(&Query) -> SatResult,
        ) -> Vec<crate::summary::Clause> {
            vec![crate::summary::Clause {
                tag: "dupchecker-fact".into(),
                formula: nonzero_ann_clause("unused").clause.formula,
                provenance: Provenance::Inferred,
            }]
        }
        fn obligations(
            &self,
            p: &Program,
            f: FuncId,
            _summary_of: &dyn Fn(FuncId) -> Summary,
        ) -> Vec<crate::checker::Obligation> {
            if p.func_name(f) != "t.Caller" {
                return Vec::new();
            }
            vec![crate::checker::Obligation {
                tag: "dupchecker-fact".into(),
                message: "call to t.Callee violates its dupchecker-fact requirement".into(),
                pos: None,
                query: Query::for_asserts(goverify_solver::Logic::All, vec![Term::bool_lit(true)]),
            }]
        }
    }

    #[test]
    fn contract_obligation_skips_when_checker_already_infers_the_same_formula() {
        // `t.Callee` carries the same "requires p0 != 0" fact twice: as
        // an annotated pragma (tag "contract") AND as DupChecker's
        // inferred clause (tag "dupchecker-fact", the same formula) — the
        // exact duplicate shape
        // `merge_dedups_duplicate_annotated_requires_keeps_distinct_one`
        // pins at the summary layer (the annotated one is deduped OUT of
        // `t.Callee`'s merged summary). `t.Caller` passes the violating
        // literal constant 0, and DupChecker's `obligations` raises its
        // own finding for it (standing in for the real checker's
        // call-site pass). Before the fix-wave item 4 fix,
        // `contract_obligations` walked the callee's raw, unmerged
        // `FuncAnnotations` and raised a SECOND (contract) finding for
        // the identical violation; the fix must yield exactly the one
        // checker finding, no contract finding.
        let mut caller = caller_calling_callee(1);
        caller.aux = vec![int_aux(1, 0)];
        let p = program_with_int_type(vec![caller]);
        let callee = p.lookup_func("t.Callee").unwrap();

        let mut funcs = BTreeMap::new();
        funcs.insert(
            callee,
            FuncAnnotations {
                requires: vec![nonzero_ann_clause("p != 0")],
                ensures: Vec::new(),
                ignores: Vec::new(),
            },
        );
        let cfg = EngineConfig {
            annotations: Annotations {
                funcs,
                findings: Vec::new(),
            },
            ..EngineConfig::default()
        };
        let checkers: Vec<&dyn Checker> = vec![&DupChecker];
        let a = analyze_full(&p, &cfg, &checkers, &z3_backend);

        let contract_findings: Vec<&Finding> = a
            .findings
            .iter()
            .filter(|f| f.checker == CONTRACT)
            .collect();
        assert!(
            contract_findings.is_empty(),
            "no contract finding when the annotation duplicates a checker-inferred \
             requires: {:?}",
            a.findings
        );
        let dup_findings: Vec<&Finding> = a
            .findings
            .iter()
            .filter(|f| f.checker == "dupchecker")
            .collect();
        assert_eq!(
            dup_findings.len(),
            1,
            "exactly one checker finding survives (no double reporting): {:?}",
            a.findings
        );
    }

    fn sig_and_int_types() -> Vec<goverify_extract::gvir::Type> {
        use goverify_extract::gvir;
        vec![
            gvir::Type {
                id: 1,
                repr: "int".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "int".into(),
                ..Default::default()
            },
            gvir::Type {
                id: 2,
                repr: "func() int".into(),
                kind: gvir::TypeKind::Signature as i32,
                results: vec![1],
                ..Default::default()
            },
        ]
    }

    /// `t.F() int { return 5 }`.
    fn returns_five() -> goverify_extract::gvir::Function {
        goverify_extract::gvir::Function {
            id: "t.F".into(),
            r#type: 2,
            aux: vec![int_aux(1, 5)],
            blocks: vec![block(
                0,
                vec![goverify_extract::gvir::Instruction {
                    kind: "Return".into(),
                    operands: vec![1],
                    ..Default::default()
                }],
                vec![],
            )],
            ..Default::default()
        }
    }

    fn ensures_ann_clause(rhs: i64, text: &str, pos: Option<Pos>) -> AnnClause {
        let r0 = Term::var("r0", Sort::BitVec(64));
        AnnClause {
            clause: crate::summary::Clause {
                tag: CONTRACT.into(),
                formula: crate::summary::Formula {
                    term: Term::eq(r0, Term::bv_lit(64, rhs as u128)).unwrap(),
                },
                provenance: Provenance::Annotated,
            },
            text: text.into(),
            pos,
        }
    }

    fn program_with_returns_five(annotations: Annotations) -> (Program, EngineConfig) {
        use goverify_extract::gvir;
        let p = Program::from_packages(vec![gvir::Package {
            import_path: "t".into(),
            types: sig_and_int_types(),
            functions: vec![returns_five()],
            ..Default::default()
        }]);
        let cfg = EngineConfig {
            annotations,
            ..EngineConfig::default()
        };
        (p, cfg)
    }

    #[test]
    fn verify_ensures_silent_when_the_body_proves_the_clause() {
        // t.F always returns 5; the annotated ensures `r0 == 5` is
        // proven Unsat(body ∧ r0 != 5) at the function's one return
        // site — no unverified-annotation finding.
        let mut funcs = BTreeMap::new();
        let (p, _) = program_with_returns_five(Annotations::default());
        let f = p.lookup_func("t.F").unwrap();
        funcs.insert(
            f,
            FuncAnnotations {
                requires: Vec::new(),
                ensures: vec![ensures_ann_clause(5, "r == 5", None)],
                ignores: Vec::new(),
            },
        );
        let (p, cfg) = program_with_returns_five(Annotations {
            funcs,
            findings: Vec::new(),
        });
        let checkers: Vec<&dyn Checker> = vec![&NoOpChecker];
        let a = analyze_full(&p, &cfg, &checkers, &z3_backend);

        assert!(
            a.findings
                .iter()
                .all(|fnd| fnd.checker != UNVERIFIED_ANNOTATION),
            "a body-proven ensures must never yield an unverified-annotation finding: {:?}",
            a.findings
        );
    }

    #[test]
    fn verify_ensures_warns_when_the_body_violates_the_clause() {
        // Same t.F (always returns 5), but the annotated ensures claims
        // `r0 == 6` — flatly contradicted by the body. Sat(body ∧ r0 !=
        // 6), so the clause is never proven: exactly one Warning at the
        // pragma's own pos (never a source position baked into the
        // message).
        let pragma_pos = Pos {
            file: "t.go".into(),
            line: 7,
            col: 2,
        };
        let mut funcs = BTreeMap::new();
        let (p, _) = program_with_returns_five(Annotations::default());
        let f = p.lookup_func("t.F").unwrap();
        funcs.insert(
            f,
            FuncAnnotations {
                requires: Vec::new(),
                ensures: vec![ensures_ann_clause(6, "r == 6", Some(pragma_pos.clone()))],
                ignores: Vec::new(),
            },
        );
        let (p, cfg) = program_with_returns_five(Annotations {
            funcs,
            findings: Vec::new(),
        });
        let checkers: Vec<&dyn Checker> = vec![&NoOpChecker];
        let a = analyze_full(&p, &cfg, &checkers, &z3_backend);

        let warnings: Vec<&Finding> = a
            .findings
            .iter()
            .filter(|fnd| fnd.checker == UNVERIFIED_ANNOTATION)
            .collect();
        assert_eq!(
            warnings.len(),
            1,
            "a body-violated ensures must yield exactly one unverified-annotation \
             finding: {:?}",
            a.findings
        );
        assert_eq!(warnings[0].severity, Severity::Warning);
        assert_eq!(
            warnings[0].pos,
            Some(pragma_pos),
            "the finding must anchor at the pragma's own position"
        );
        assert!(
            !warnings[0].message.contains(".go:"),
            "INVARIANT: message must never embed a source position: {:?}",
            warnings[0]
        );
    }

    /// `<var_name> > 0` (signed, BitVec-64), tag "contract"/Annotated:
    /// shared by the own-requires-as-assumption regression test for both
    /// the requires clause (over `p0`) and the ensures clause (over
    /// `r0`) — same shape, different free var.
    fn positive_ann_clause(var_name: &str, text: &str) -> AnnClause {
        let v = Term::var(var_name, Sort::BitVec(64));
        AnnClause {
            clause: crate::summary::Clause {
                tag: CONTRACT.into(),
                formula: crate::summary::Formula {
                    term: Term::bv_cmp(goverify_solver::BvCmpOp::Slt, Term::bv_lit(64, 0), v)
                        .unwrap(),
                },
                provenance: Provenance::Annotated,
            },
            text: text.into(),
            pos: None,
        }
    }

    /// `t.G(x int) int { return x }`.
    fn returns_param() -> goverify_extract::gvir::Function {
        goverify_extract::gvir::Function {
            id: "t.G".into(),
            r#type: 2,
            params: vec![goverify_extract::gvir::Param {
                id: 1,
                name: "x".into(),
                r#type: 1,
            }],
            blocks: vec![block(
                0,
                vec![goverify_extract::gvir::Instruction {
                    kind: "Return".into(),
                    operands: vec![1],
                    ..Default::default()
                }],
                vec![],
            )],
            ..Default::default()
        }
    }

    #[test]
    fn verify_ensures_uses_own_requires_as_an_assumption() {
        // //goverify:requires x > 0
        // //goverify:ensures ret > 0
        // func F(x int) int { return x }
        //
        // `ret > 0` is NOT provable from the body alone (x can be <= 0
        // in general) — it is only provable by ASSUMING the function's
        // own annotated requires (`x > 0`) alongside the body, per the
        // design spec's requires-assumed-at-entry rule (§1(a)), the same
        // way the checkers' own in-body obligations assume
        // `own_preconditions`. Regression test for the reviewer-flagged
        // finding: `verify_ensures` must conjoin `own_requires` into the
        // query, not just `body ∧ ¬clause` — without the fix this would
        // spuriously warn (Sat via x = -1).
        use goverify_extract::gvir;
        let p = Program::from_packages(vec![gvir::Package {
            import_path: "t".into(),
            types: sig_and_int_types(),
            functions: vec![returns_param()],
            ..Default::default()
        }]);
        let f = p.lookup_func("t.G").unwrap();

        let mut funcs = BTreeMap::new();
        funcs.insert(
            f,
            FuncAnnotations {
                requires: vec![positive_ann_clause("p0", "x > 0")],
                ensures: vec![positive_ann_clause("r0", "ret > 0")],
                ignores: Vec::new(),
            },
        );
        let cfg = EngineConfig {
            annotations: Annotations {
                funcs,
                findings: Vec::new(),
            },
            ..EngineConfig::default()
        };
        let checkers: Vec<&dyn Checker> = vec![&NoOpChecker];
        let a = analyze_full(&p, &cfg, &checkers, &z3_backend);

        assert!(
            a.findings
                .iter()
                .all(|fnd| fnd.checker != UNVERIFIED_ANNOTATION),
            "an ensures clause provable under the function's own annotated requires \
             must never yield an unverified-annotation finding: {:?}",
            a.findings
        );
    }

    #[test]
    fn annotation_findings_replay_identically_from_a_warm_scc_cache() {
        // One program exercising BOTH annotation passes at once (a
        // violating contract call site + an unprovable ensures), run
        // twice through the same on-disk SCC cache: the second run must
        // replay byte-identical findings (contract AND
        // unverified-annotation alike) without recomputing them, exactly
        // like the existing checker-finding cache tests.
        let mut caller = caller_calling_callee(1);
        caller.aux = vec![int_aux(1, 0)];
        let ensures_fn = {
            let mut f = returns_five();
            f.id = "t.EnsF".into();
            f
        };
        let p0 = Program::from_packages(vec![goverify_extract::gvir::Package {
            import_path: "t".into(),
            types: sig_and_int_types(),
            functions: vec![caller, ensures_fn],
            ..Default::default()
        }]);
        let callee = p0.lookup_func("t.Callee").unwrap();
        let ens_f = p0.lookup_func("t.EnsF").unwrap();

        let mut funcs = BTreeMap::new();
        funcs.insert(
            callee,
            FuncAnnotations {
                requires: vec![nonzero_ann_clause("p != 0")],
                ensures: Vec::new(),
                ignores: Vec::new(),
            },
        );
        funcs.insert(
            ens_f,
            FuncAnnotations {
                requires: Vec::new(),
                ensures: vec![ensures_ann_clause(6, "r == 6", None)],
                ignores: Vec::new(),
            },
        );
        let annotations = Annotations {
            funcs,
            findings: Vec::new(),
        };

        let dir = tempfile::tempdir().unwrap();
        let build_cfg = || EngineConfig {
            cache_dir: Some(dir.path().to_path_buf()),
            annotations: annotations.clone(),
            ..EngineConfig::default()
        };
        let checkers: Vec<&dyn Checker> = vec![&NoOpChecker];

        let a1 = analyze_full(&p0, &build_cfg(), &checkers, &z3_backend);
        assert_eq!(
            a1.findings.iter().filter(|f| f.checker == CONTRACT).count(),
            1,
            "sanity: the contract finding fires on the cold run: {:?}",
            a1.findings
        );
        assert_eq!(
            a1.findings
                .iter()
                .filter(|f| f.checker == UNVERIFIED_ANNOTATION)
                .count(),
            1,
            "sanity: the unverified-annotation finding fires on the cold run: {:?}",
            a1.findings
        );

        let a2 = analyze_full(&p0, &build_cfg(), &checkers, &z3_backend);
        assert_eq!(
            a1.findings, a2.findings,
            "warm-replayed annotation findings must be byte-identical to the cold run"
        );
        assert!(
            a2.scc_cache_hits > 0,
            "the second run must actually replay from the SCC cache: hits={} misses={}",
            a2.scc_cache_hits,
            a2.scc_cache_misses
        );
    }
}
