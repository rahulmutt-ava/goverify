//! Per-SCC analysis cache (phase-5a spec §2/§4): key = recursive
//! content hash over the condensed call DAG; value = summaries +
//! findings + diagnostics for every member, so a hit replays
//! byte-identical output without encoding or solving.
//!
//! Lives here, not in goverify-cache, because the entry payload IS
//! analysis meaning (Summary/Finding/Term); goverify-cache stays
//! bytes-only (spec Deviation note, plan header).

use std::path::PathBuf;

use goverify_cache::Store;
use goverify_ir::{Pos, Program, Sccs};
use goverify_solver::{DatatypeDecl, SolverLimits, decode_term, encode_term, ptr_datatype};

use crate::checker::{Finding, TraceStep};
use crate::effects::{ChanOp, Effects, Loc, LockOp, Root, Spawns};
use crate::encode::seq_datatype;
use crate::summary::{Clause, Formula, Provenance, Summary};

/// Bump on: entry-format change, any engine/encoding semantic change,
/// prost major bump, CLI RETRY_FACTOR change (escalated limits are
/// derived from base limits and deliberately not keyed separately).
const SCC_CACHE_VERSION: u32 = 1;
const LAYER: &str = "scc";
const SCC_ENTRY_FORMAT: u8 = 1;

/// Length-prefixes `b` into the salt hash. A free function (not a
/// closure over `h`) so it can be called interleaved with direct
/// `h.update()` calls in `SccCache::open` without a double-mutable-borrow
/// conflict.
fn hash_field(h: &mut blake3::Hasher, b: &[u8]) {
    h.update(&(b.len() as u64).to_le_bytes());
    h.update(b);
}

#[derive(Debug, Clone)]
pub struct CacheConfigKey {
    pub solver_identity: String,
    pub infer_limits: SolverLimits,
    pub findings_limits: SolverLimits,
    pub widen_after: u32,
    /// (checker name, checker version), sorted by name.
    pub checkers: Vec<(&'static str, u32)>,
}

pub struct SccCache {
    store: Store,
    salt: [u8; 32],
}

impl SccCache {
    pub fn open(root: PathBuf, cfg: &CacheConfigKey) -> SccCache {
        let mut h = blake3::Hasher::new();
        h.update(b"goverify-scc-salt\0");
        h.update(&SCC_CACHE_VERSION.to_le_bytes());
        h.update(&[goverify_solver::TERM_CODEC_VERSION]);
        hash_field(&mut h, cfg.solver_identity.as_bytes());
        h.update(&cfg.infer_limits.timeout_ms.to_le_bytes());
        h.update(&cfg.infer_limits.mem_mb.to_le_bytes());
        h.update(&cfg.findings_limits.timeout_ms.to_le_bytes());
        h.update(&cfg.findings_limits.mem_mb.to_le_bytes());
        h.update(&cfg.widen_after.to_le_bytes());
        let mut checkers = cfg.checkers.clone();
        checkers.sort();
        for (name, version) in &checkers {
            hash_field(&mut h, name.as_bytes());
            h.update(&version.to_le_bytes());
        }
        SccCache {
            store: Store::open(root),
            salt: *h.finalize().as_bytes(),
        }
    }

    /// Schedule-order keys; callees-first order guarantees callee keys
    /// exist when needed. Member hashes and callee keys are sorted
    /// BYTEWISE before hashing: FuncId/schedule numbering must never
    /// reach a key (ids shift when unrelated functions appear).
    pub fn keys(&self, p: &Program, sccs: &Sccs) -> Vec<[u8; 32]> {
        let n = sccs.schedule().len();
        let mut keys: Vec<[u8; 32]> = Vec::with_capacity(n);
        for si in 0..n {
            let mut members: Vec<[u8; 32]> = sccs.schedule()[si]
                .iter()
                .map(|&m| p.func_ir_hash(m))
                .collect();
            members.sort_unstable();
            let mut callees: Vec<[u8; 32]> =
                sccs.callee_sccs(si).iter().map(|&d| keys[d]).collect();
            callees.sort_unstable();
            let mut h = blake3::Hasher::new();
            h.update(b"goverify-scc-key\0");
            h.update(&self.salt);
            h.update(&(members.len() as u64).to_le_bytes());
            for m in &members {
                h.update(m);
            }
            h.update(&(callees.len() as u64).to_le_bytes());
            for c in &callees {
                h.update(c);
            }
            keys.push(*h.finalize().as_bytes());
        }
        keys
    }

    pub fn get(&self, key: &[u8; 32]) -> Option<SccEntry> {
        decode_entry_bytes(&self.store.get(LAYER, key)?)
    }

    pub fn put(&self, key: &[u8; 32], e: &SccEntry) -> std::io::Result<()> {
        self.store.put(LAYER, key, &encode_entry(e))
    }

    #[cfg(test)]
    pub(crate) fn salt_for_test(&self) -> [u8; 32] {
        self.salt
    }
}

/// One SCC's cached analysis output, in schedule order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SccEntry {
    pub members: Vec<MemberEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberEntry {
    /// SSA function id — integrity check on decode-install.
    pub func: String,
    pub summary: Summary,
    /// The `diag_slots` entry, if any.
    pub analysis_diag: Option<String>,
    /// Already (pos, message)-sorted.
    pub findings: Vec<Finding>,
    /// Encode-skip / panic diagnostics, in emit order.
    pub findings_diags: Vec<String>,
}

// ---- primitives (Task 5's codec.rs style, reimplemented locally) ----

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend(v.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_u32(out, s.len() as u32);
    out.extend(s.as_bytes());
}

fn take_u8(input: &mut &[u8]) -> Option<u8> {
    let (b, rest) = input.split_first()?;
    *input = rest;
    Some(*b)
}

fn take_u32(input: &mut &[u8]) -> Option<u32> {
    let (bytes, rest) = input.split_first_chunk::<4>()?;
    *input = rest;
    Some(u32::from_le_bytes(*bytes))
}

fn take_str(input: &mut &[u8]) -> Option<String> {
    let len = take_u32(input)? as usize;
    if input.len() < len {
        return None;
    }
    let (s, rest) = input.split_at(len);
    *input = rest;
    String::from_utf8(s.to_vec()).ok()
}

/// Bound a length-prefixed count against remaining input before any
/// allocation (same defensive pattern as `decode_term`'s `decode_many`).
fn take_count(input: &mut &[u8]) -> Option<usize> {
    let n = take_u32(input)? as usize;
    if n > input.len() {
        return None;
    }
    Some(n)
}

fn term_decls() -> Vec<DatatypeDecl> {
    vec![ptr_datatype(), seq_datatype()]
}

// ---- Provenance ----

fn encode_provenance(p: &Provenance, out: &mut Vec<u8>) {
    out.push(match p {
        Provenance::Inferred => 0,
        Provenance::Havoc => 1,
    });
}

fn decode_provenance(input: &mut &[u8]) -> Option<Provenance> {
    match take_u8(input)? {
        0 => Some(Provenance::Inferred),
        1 => Some(Provenance::Havoc),
        _ => None,
    }
}

// ---- Clause / Formula ----

fn encode_clause(c: &Clause, out: &mut Vec<u8>) {
    put_str(out, &c.tag);
    encode_term(&c.formula.term, out);
}

fn decode_clause(input: &mut &[u8], decls: &[DatatypeDecl]) -> Option<Clause> {
    let tag = take_str(input)?;
    let term = decode_term(input, decls)?;
    Some(Clause {
        tag,
        formula: Formula { term },
    })
}

fn encode_clauses(cs: &[Clause], out: &mut Vec<u8>) {
    put_u32(out, cs.len() as u32);
    for c in cs {
        encode_clause(c, out);
    }
}

fn decode_clauses(input: &mut &[u8], decls: &[DatatypeDecl]) -> Option<Vec<Clause>> {
    let n = take_count(input)?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(decode_clause(input, decls)?);
    }
    Some(out)
}

// ---- Spawns / ChanOp / LockOp ----

fn encode_spawns(s: Spawns, out: &mut Vec<u8>) {
    out.push(match s {
        Spawns::None => 0,
        Spawns::Bounded => 1,
        Spawns::Unbounded => 2,
    });
}

fn decode_spawns(input: &mut &[u8]) -> Option<Spawns> {
    match take_u8(input)? {
        0 => Some(Spawns::None),
        1 => Some(Spawns::Bounded),
        2 => Some(Spawns::Unbounded),
        _ => None,
    }
}

fn encode_chan_op(op: ChanOp, out: &mut Vec<u8>) {
    out.push(match op {
        ChanOp::Make => 0,
        ChanOp::Send => 1,
        ChanOp::Recv => 2,
        ChanOp::Close => 3,
        ChanOp::Select => 4,
    });
}

fn decode_chan_op(input: &mut &[u8]) -> Option<ChanOp> {
    match take_u8(input)? {
        0 => Some(ChanOp::Make),
        1 => Some(ChanOp::Send),
        2 => Some(ChanOp::Recv),
        3 => Some(ChanOp::Close),
        4 => Some(ChanOp::Select),
        _ => None,
    }
}

fn encode_lock_op(op: LockOp, out: &mut Vec<u8>) {
    out.push(match op {
        LockOp::Lock => 0,
        LockOp::Unlock => 1,
        LockOp::RLock => 2,
        LockOp::RUnlock => 3,
        LockOp::DeferredUnlock => 4,
        LockOp::DeferredRUnlock => 5,
    });
}

fn decode_lock_op(input: &mut &[u8]) -> Option<LockOp> {
    match take_u8(input)? {
        0 => Some(LockOp::Lock),
        1 => Some(LockOp::Unlock),
        2 => Some(LockOp::RLock),
        3 => Some(LockOp::RUnlock),
        4 => Some(LockOp::DeferredUnlock),
        5 => Some(LockOp::DeferredRUnlock),
        _ => None,
    }
}

// ---- Root / Loc ----

fn encode_root(r: &Root, out: &mut Vec<u8>) {
    match r {
        Root::Param(i) => {
            out.push(0);
            put_u32(out, *i);
        }
        Root::Global(name) => {
            out.push(1);
            put_str(out, name);
        }
        Root::Alloc(i) => {
            out.push(2);
            put_u32(out, *i);
        }
        Root::Unknown => out.push(3),
    }
}

fn decode_root(input: &mut &[u8]) -> Option<Root> {
    match take_u8(input)? {
        0 => Some(Root::Param(take_u32(input)?)),
        1 => Some(Root::Global(take_str(input)?)),
        2 => Some(Root::Alloc(take_u32(input)?)),
        3 => Some(Root::Unknown),
        _ => None,
    }
}

fn encode_loc(l: &Loc, out: &mut Vec<u8>) {
    encode_root(&l.root, out);
    put_u32(out, l.path.len() as u32);
    for p in &l.path {
        put_u32(out, *p);
    }
}

fn decode_loc(input: &mut &[u8]) -> Option<Loc> {
    let root = decode_root(input)?;
    let n = take_count(input)?;
    let mut path = Vec::with_capacity(n);
    for _ in 0..n {
        path.push(take_u32(input)?);
    }
    Some(Loc { root, path })
}

// ---- Effects ----
//
// Iterates the two BTreeMaps directly (already deterministically
// ordered by `Loc`'s `Ord`); decode reinserts entries in the same
// order.

fn encode_effects(e: &Effects, out: &mut Vec<u8>) {
    encode_spawns(e.spawns, out);
    put_u32(out, e.chan_ops.len() as u32);
    for (loc, ops) in &e.chan_ops {
        encode_loc(loc, out);
        put_u32(out, ops.len() as u32);
        for &op in ops {
            encode_chan_op(op, out);
        }
    }
    put_u32(out, e.lock_ops.len() as u32);
    for (loc, ops) in &e.lock_ops {
        encode_loc(loc, out);
        put_u32(out, ops.len() as u32);
        for &op in ops {
            encode_lock_op(op, out);
        }
    }
}

fn decode_effects(input: &mut &[u8]) -> Option<Effects> {
    let spawns = decode_spawns(input)?;
    let mut chan_ops = std::collections::BTreeMap::new();
    let n_chan = take_count(input)?;
    for _ in 0..n_chan {
        let loc = decode_loc(input)?;
        let n_ops = take_count(input)?;
        let mut ops = std::collections::BTreeSet::new();
        for _ in 0..n_ops {
            ops.insert(decode_chan_op(input)?);
        }
        chan_ops.insert(loc, ops);
    }
    let mut lock_ops = std::collections::BTreeMap::new();
    let n_lock = take_count(input)?;
    for _ in 0..n_lock {
        let loc = decode_loc(input)?;
        let n_ops = take_count(input)?;
        let mut ops = std::collections::BTreeSet::new();
        for _ in 0..n_ops {
            ops.insert(decode_lock_op(input)?);
        }
        lock_ops.insert(loc, ops);
    }
    Some(Effects {
        spawns,
        chan_ops,
        lock_ops,
    })
}

// ---- Summary ----

fn encode_summary(s: &Summary, out: &mut Vec<u8>) {
    encode_clauses(&s.requires, out);
    encode_clauses(&s.ensures, out);
    encode_effects(&s.effects, out);
    encode_provenance(&s.provenance, out);
}

fn decode_summary(input: &mut &[u8], decls: &[DatatypeDecl]) -> Option<Summary> {
    let requires = decode_clauses(input, decls)?;
    let ensures = decode_clauses(input, decls)?;
    let effects = decode_effects(input)?;
    let provenance = decode_provenance(input)?;
    Some(Summary {
        requires,
        ensures,
        effects,
        provenance,
    })
}

// ---- Pos / Option<Pos> ----

fn encode_pos(pos: &Pos, out: &mut Vec<u8>) {
    put_str(out, &pos.file);
    put_u32(out, pos.line);
    put_u32(out, pos.col);
}

fn decode_pos(input: &mut &[u8]) -> Option<Pos> {
    let file = take_str(input)?;
    let line = take_u32(input)?;
    let col = take_u32(input)?;
    Some(Pos { file, line, col })
}

fn encode_opt_pos(pos: &Option<Pos>, out: &mut Vec<u8>) {
    match pos {
        Some(p) => {
            out.push(1);
            encode_pos(p, out);
        }
        None => out.push(0),
    }
}

fn decode_opt_pos(input: &mut &[u8]) -> Option<Option<Pos>> {
    match take_u8(input)? {
        0 => Some(None),
        1 => Some(Some(decode_pos(input)?)),
        _ => None,
    }
}

// ---- TraceStep ----

fn encode_trace_step(t: &TraceStep, out: &mut Vec<u8>) {
    put_u32(out, t.block);
    encode_opt_pos(&t.pos, out);
}

fn decode_trace_step(input: &mut &[u8]) -> Option<TraceStep> {
    let block = take_u32(input)?;
    let pos = decode_opt_pos(input)?;
    Some(TraceStep { block, pos })
}

// ---- Finding ----

fn encode_finding(f: &Finding, out: &mut Vec<u8>) {
    put_str(out, &f.checker);
    put_str(out, &f.tag);
    put_str(out, &f.func);
    encode_opt_pos(&f.pos, out);
    put_str(out, &f.message);
    put_u32(out, f.trace.len() as u32);
    for t in &f.trace {
        encode_trace_step(t, out);
    }
    put_u32(out, f.model.len() as u32);
    for (k, v) in &f.model {
        put_str(out, k);
        put_str(out, v);
    }
}

fn decode_finding(input: &mut &[u8]) -> Option<Finding> {
    let checker = take_str(input)?;
    let tag = take_str(input)?;
    let func = take_str(input)?;
    let pos = decode_opt_pos(input)?;
    let message = take_str(input)?;
    let n_trace = take_count(input)?;
    let mut trace = Vec::with_capacity(n_trace);
    for _ in 0..n_trace {
        trace.push(decode_trace_step(input)?);
    }
    let n_model = take_count(input)?;
    let mut model = Vec::with_capacity(n_model);
    for _ in 0..n_model {
        let k = take_str(input)?;
        let v = take_str(input)?;
        model.push((k, v));
    }
    Some(Finding {
        checker,
        tag,
        func,
        pos,
        message,
        trace,
        model,
    })
}

// ---- MemberEntry / SccEntry ----

fn encode_member(m: &MemberEntry, out: &mut Vec<u8>) {
    put_str(out, &m.func);
    encode_summary(&m.summary, out);
    match &m.analysis_diag {
        Some(d) => {
            out.push(1);
            put_str(out, d);
        }
        None => out.push(0),
    }
    put_u32(out, m.findings.len() as u32);
    for f in &m.findings {
        encode_finding(f, out);
    }
    put_u32(out, m.findings_diags.len() as u32);
    for d in &m.findings_diags {
        put_str(out, d);
    }
}

fn decode_member(input: &mut &[u8], decls: &[DatatypeDecl]) -> Option<MemberEntry> {
    let func = take_str(input)?;
    let summary = decode_summary(input, decls)?;
    let analysis_diag = match take_u8(input)? {
        0 => None,
        1 => Some(take_str(input)?),
        _ => return None,
    };
    let n_findings = take_count(input)?;
    let mut findings = Vec::with_capacity(n_findings);
    for _ in 0..n_findings {
        findings.push(decode_finding(input)?);
    }
    let n_diags = take_count(input)?;
    let mut findings_diags = Vec::with_capacity(n_diags);
    for _ in 0..n_diags {
        findings_diags.push(take_str(input)?);
    }
    Some(MemberEntry {
        func,
        summary,
        analysis_diag,
        findings,
        findings_diags,
    })
}

/// Encodes `e` in the `SCC_ENTRY_FORMAT` layout: version byte, u32-LE
/// member count, then each member's length-prefixed fields.
fn encode_entry(e: &SccEntry) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(SCC_ENTRY_FORMAT);
    put_u32(&mut out, e.members.len() as u32);
    for m in &e.members {
        encode_member(m, &mut out);
    }
    out
}

/// Fuzz surface (Task 13): decode arbitrary bytes, never panic. Rejects
/// unless `bytes` is fully consumed by a well-formed entry (trailing
/// garbage = miss).
pub fn decode_entry_bytes(bytes: &[u8]) -> Option<SccEntry> {
    let mut input = bytes;
    match take_u8(&mut input)? {
        SCC_ENTRY_FORMAT => {}
        _ => return None,
    }
    let decls = term_decls();
    let n = take_count(&mut input)?;
    let mut members = Vec::with_capacity(n);
    for _ in 0..n {
        members.push(decode_member(&mut input, &decls)?);
    }
    if !input.is_empty() {
        return None;
    }
    Some(SccEntry { members })
}

#[cfg(test)]
mod tests {
    use goverify_solver::{Sort, Term};

    use super::*;
    use crate::summary::{Clause, Formula, Provenance, Summary};

    fn sample_entry() -> SccEntry {
        let term = Term::eq(Term::var("p0", Sort::BitVec(64)), Term::bv_lit(64, 0)).unwrap();
        SccEntry {
            members: vec![MemberEntry {
                func: "example.com/m.F".to_string(),
                summary: Summary {
                    requires: vec![Clause {
                        tag: "nil-deref".to_string(),
                        formula: Formula { term: term.clone() },
                    }],
                    ensures: vec![],
                    effects: crate::Effects::top(),
                    provenance: Provenance::Inferred,
                },
                analysis_diag: Some("widened".to_string()),
                findings: vec![crate::Finding {
                    checker: "nil".to_string(),
                    tag: "nil-deref".to_string(),
                    func: "example.com/m.F".to_string(),
                    pos: Some(goverify_ir::Pos {
                        file: "m.go".to_string(),
                        line: 3,
                        col: 9,
                    }),
                    message: "possible nil dereference".to_string(),
                    trace: vec![crate::TraceStep {
                        block: 0,
                        pos: None,
                    }],
                    model: vec![("p0".to_string(), "(ptr-nil)".to_string())],
                }],
                findings_diags: vec!["skipped encode".to_string()],
            }],
        }
    }

    #[test]
    fn entry_round_trips() {
        let e = sample_entry();
        let bytes = encode_entry(&e);
        let back = decode_entry_bytes(&bytes).expect("decode_entry_bytes()");
        assert_eq!(back.members.len(), 1);
        let (a, b) = (&back.members[0], &e.members[0]);
        assert_eq!(a.func, b.func);
        assert_eq!(
            a.summary, b.summary,
            "Summary round-trip incl. Effects/Terms"
        );
        assert_eq!(a.analysis_diag, b.analysis_diag);
        assert_eq!(a.findings, b.findings);
        assert_eq!(a.findings_diags, b.findings_diags);
    }

    #[test]
    fn corrupt_entries_are_none_never_panic() {
        let bytes = encode_entry(&sample_entry());
        for cut in 0..bytes.len() {
            let _ = decode_entry_bytes(&bytes[..cut]); // no panic
        }
        let mut garbled = bytes.clone();
        garbled.push(0); // trailing garbage
        assert!(
            decode_entry_bytes(&garbled).is_none(),
            "trailing garbage = miss"
        );
        assert!(decode_entry_bytes(&[]).is_none());
        assert!(decode_entry_bytes(&[0xff; 8]).is_none());
    }

    #[test]
    fn store_round_trip_and_key_shape() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = CacheConfigKey {
            solver_identity: "stub".to_string(),
            infer_limits: goverify_solver::SolverLimits::default(),
            findings_limits: goverify_solver::SolverLimits::default(),
            widen_after: 3,
            checkers: vec![("nil", 1)],
        };
        let c = SccCache::open(dir.path().to_path_buf(), &cfg);
        let key = [9u8; 32];
        assert!(c.get(&key).is_none(), "empty cache misses");
        c.put(&key, &sample_entry()).unwrap();
        assert!(c.get(&key).is_some(), "round-trips through Store");

        // Different config = different salt = disjoint keys for the
        // same program. Checked indirectly: two caches with different
        // identities must produce different keys() for one program.
        // (Program-level keys() coverage lives in Task 8/9's
        // integration tests; here we only pin salt sensitivity.)
        let cfg2 = CacheConfigKey {
            solver_identity: "other".to_string(),
            ..cfg
        };
        let c2 = SccCache::open(dir.path().to_path_buf(), &cfg2);
        assert_ne!(
            c.salt_for_test(),
            c2.salt_for_test(),
            "identity is in the salt"
        );
    }
}
