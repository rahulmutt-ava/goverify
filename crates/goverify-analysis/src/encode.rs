//! Gated-SSA function encoding (phase-4 spec §3): one whole-function
//! SMT encoding — block guards, phi-as-ite, back edges cut (loops are
//! analyzed as their first iteration; reachability stays
//! under-approximate, so Sat always witnesses a genuine execution).
//! Pure: `Program` in, declarations + assertions out. Anything not
//! modeled havocs — degrade, never die.

use std::collections::{BTreeMap, BTreeSet};

use goverify_ir::{
    BinOpKind, Callee, ConstVal, FuncId, Function, MakeKind, Op, Program, TypeId, TypeKind,
    TypeTable, UnOpKind, ValueId, ValueKind,
};
use goverify_solver::{
    BvBinOp, BvCmpOp, CtorDecl, DatatypeDecl, Logic, Query, Sort, Term, ptr_datatype, ptr_is_nil,
    ptr_nil, ptr_sort,
};

/// Slices/strings as length-carrying opaque values: contents havoc,
/// bounds reasoning uses len/cap only. Invariant 0 <= len <= cap is
/// asserted for every Seq-sorted value the encoder introduces.
///
/// Named `GoSeq`, NOT `Seq`: under `(set-logic ALL)` Z3 pre-registers a
/// builtin sort literally named `Seq` (its native sequence theory), so
/// `(declare-datatypes ((Seq 0)) ...)` fails to parse ("sort already
/// defined Seq") — every `discharge_query` call then sees a non-Ok error
/// code, resets the context, and reports `Unknown`, silently swallowing
/// every finding that touches a slice/string-typed value. Caught here
/// because Task 6 is the first caller to actually discharge an
/// `encode_func`-produced query through Z3Native.
pub fn seq_datatype() -> DatatypeDecl {
    DatatypeDecl {
        name: "GoSeq".into(),
        ctors: vec![CtorDecl {
            name: "seq-val".into(),
            fields: vec![
                ("seq-len".into(), Sort::BitVec(64)),
                ("seq-cap".into(), Sort::BitVec(64)),
            ],
        }],
    }
}

/// (width, signed) for integer basic types; None for anything else
/// (floats/complex are NOT ints — phase-3 final-review I1).
pub fn int_repr(types: &TypeTable, t: TypeId) -> Option<(u32, bool)> {
    match types.kind(t) {
        TypeKind::Named { underlying, .. } => int_repr(types, *underlying),
        TypeKind::Basic { name } => match name.as_str() {
            "int" | "int64" => Some((64, true)),
            "int32" | "rune" => Some((32, true)),
            "int16" => Some((16, true)),
            "int8" => Some((8, true)),
            "uint" | "uint64" | "uintptr" => Some((64, false)),
            "uint32" => Some((32, false)),
            "uint16" => Some((16, false)),
            "uint8" | "byte" => Some((8, false)),
            _ => None,
        },
        _ => None,
    }
}

/// Basic-type name after peeling Named wrappers; None for non-basic.
fn basic_name(types: &TypeTable, t: TypeId) -> Option<&str> {
    match types.kind(t) {
        TypeKind::Named { underlying, .. } => basic_name(types, *underlying),
        TypeKind::Basic { name } => Some(name),
        _ => None,
    }
}

/// The static length backing `ty`, when `ty` is itself an array OR a
/// pointer to one (through any number of `Named` wrappers on either
/// side, mirroring `int_repr`/`sort_of`'s own resolution — a `type Arr
/// [3]int` or a `*Arr` must resolve exactly like `[3]int`/`*[3]int`):
/// go/ssa commonly addresses arrays through their pointer — a plain
/// `var a [N]T` is still typed `*[N]T` at every `IndexAddr`/`Slice` site
/// that reads it, and `make([]T, n)` with a compile-time-constant `n`
/// lowers to `Alloc` (a fresh `*[n]T`) plus a whole-array `Slice`, never
/// `MakeSlice` (Task 8 corpus finding). Peels exactly one pointer level
/// — `None` for anything else (a genuine Seq value's length lives in
/// its own `seq-len`/`seq-cap` term instead).
pub fn array_len(types: &TypeTable, ty: TypeId) -> Option<u64> {
    match types.kind(ty) {
        TypeKind::Named { underlying, .. } => array_len(types, *underlying),
        TypeKind::Array { len, .. } => Some(*len),
        TypeKind::Pointer { elem } => array_len_direct(types, *elem),
        _ => None,
    }
}

/// `array_len`'s pointer-elem resolution: `elem` must itself resolve
/// (through `Named`) to an array — NOT recursively through another
/// pointer (`**[N]T` doesn't back an addressable `[N]T` the way `*[N]T`
/// does).
fn array_len_direct(types: &TypeTable, ty: TypeId) -> Option<u64> {
    match types.kind(ty) {
        TypeKind::Named { underlying, .. } => array_len_direct(types, *underlying),
        TypeKind::Array { len, .. } => Some(*len),
        _ => None,
    }
}

/// TypeId -> SMT sort for the v1 theory surface. None = value havocs
/// (no term). `int`/`uint`/`uintptr` are 64-bit (64-bit targets only in
/// v1 — documented degrade).
pub fn sort_of(types: &TypeTable, t: TypeId) -> Option<Sort> {
    if let Some((w, _)) = int_repr(types, t) {
        return Some(Sort::BitVec(w));
    }
    match types.kind(t) {
        TypeKind::Named { underlying, .. } => sort_of(types, *underlying),
        TypeKind::Basic { name } if name == "bool" => Some(Sort::Bool),
        TypeKind::Basic { name } if name == "string" => Some(seq_datatype().sort()),
        TypeKind::Pointer { .. } => Some(ptr_sort()),
        TypeKind::Slice { .. } => Some(seq_datatype().sort()),
        _ => None,
    }
}

/// DFS edge classification from the entry block: returns `succs` with
/// back edges (target on the current DFS stack) removed. Unreachable
/// blocks keep their edges (harmless: `encode_guards` forces their guard
/// to false — see `cut_back_edges_visited`). Public signature FROZEN;
/// callers needing the reachability flags use `cut_back_edges_visited`.
pub fn cut_back_edges(f: &Function) -> Vec<Vec<u32>> {
    cut_back_edges_visited(f).0
}

/// `cut_back_edges` plus the DFS reachability flags: `visited[b]` is true
/// iff block `b` is reachable from the entry (block 0). `encode_guards`
/// consumes these to assert `g_b = false` for every UNREACHABLE block,
/// closing a crafted-/degraded-.gvir false-positive hole: the DFS only
/// classifies edges reachable from block 0, so an unreachable cycle keeps
/// mutually-supporting guard equations (`g1 = g2`, `g2 = g1`) that a
/// solver can satisfy with both true, letting `reach_query` on an
/// unreachable block return Sat. go/ssa prunes unreachable blocks, so
/// only the untrusted-input surface is affected — but it is still a real
/// no-FP hole.
pub(crate) fn cut_back_edges_visited(f: &Function) -> (Vec<Vec<u32>>, Vec<bool>) {
    let n = f.blocks.len();
    let mut dag: Vec<Vec<u32>> = f.blocks.iter().map(|b| b.succs.clone()).collect();
    let mut state = vec![0u8; n]; // 0 unvisited, 1 on stack, 2 done
    // Iterative DFS; (block, next-succ-index) frames.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    if n == 0 {
        return (dag, Vec::new());
    }
    stack.push((0, 0));
    state[0] = 1;
    let mut cut: Vec<(usize, u32)> = Vec::new();
    while let Some(frame) = stack.last_mut() {
        let (b, i) = (frame.0, frame.1);
        let succs = &f.blocks[b].succs;
        if i < succs.len() {
            let s = succs[i] as usize;
            frame.1 += 1;
            if s >= n {
                continue; // raw wire id out of range: ignore edge (degrade)
            }
            match state[s] {
                0 => {
                    state[s] = 1;
                    stack.push((s, 0));
                }
                1 => cut.push((b, s as u32)), // back edge
                _ => {}
            }
        } else {
            state[b] = 2;
            stack.pop();
        }
    }
    for (b, s) in cut {
        dag[b].retain(|&x| x != s);
    }
    // A block ends the DFS `done` (state 2) iff it was reached; anything
    // still `unvisited` (state 0) is unreachable from the entry.
    let visited = state.iter().map(|&s| s != 0).collect();
    (dag, visited)
}

/// Term-count budget: a function whose encoding exceeds this many
/// assertions is skipped with a diagnostic (spec §3 size cap).
const ASSERT_CAP: usize = 50_000;

pub struct EncodedFunc {
    pub datatypes: Vec<DatatypeDecl>,
    pub consts: Vec<(String, Sort)>,
    pub asserts: Vec<Term>,
    pub guards: Vec<Term>,
    pub values: BTreeMap<ValueId, Term>,
    pub dag_succs: Vec<Vec<u32>>,
}

impl EncodedFunc {
    pub fn value(&self, v: ValueId) -> Option<&Term> {
        self.values.get(&v)
    }

    /// One canonical obligation query: everything the encoding asserts,
    /// plus reach(block), plus the caller's extra conjuncts (violation,
    /// preconditions). Consts are sorted for determinism.
    pub fn reach_query(&self, block: usize, extra: Vec<Term>) -> Query {
        let mut asserts = self.asserts.clone();
        if let Some(g) = self.guards.get(block) {
            asserts.push(g.clone());
        }
        asserts.extend(extra);
        let mut consts = self.consts.clone();
        consts.sort();
        Query {
            logic: Logic::All,
            datatypes: self.datatypes.clone(),
            consts,
            asserts,
        }
    }
}

fn guard_name(b: usize) -> String {
    format!("g{b}")
}

fn value_name(f: &Function, v: ValueId) -> String {
    // Params use the iface convention p<i> so requires clauses and
    // call-site instantiation share the encoder's terms directly.
    if let Some(i) = f.params.iter().position(|&pv| pv == v) {
        return crate::summary::iface_var_name(&crate::summary::IfaceVar::Param(i as u32));
    }
    format!("v{}", v.0)
}

pub fn encode_func(p: &Program, f: FuncId) -> Result<EncodedFunc, String> {
    let func = p
        .func(f)
        .ok_or_else(|| format!("{}: no body to encode", p.func_name(f)))?;
    let n = func.blocks.len();
    let (dag_succs, reachable) = cut_back_edges_visited(func);
    let mut enc = EncodedFunc {
        datatypes: vec![ptr_datatype(), seq_datatype()],
        consts: Vec::new(),
        asserts: Vec::new(),
        guards: (0..n)
            .map(|b| Term::var(&guard_name(b), Sort::Bool))
            .collect(),
        values: BTreeMap::new(),
        dag_succs,
    };
    for b in 0..n {
        enc.consts.push((guard_name(b), Sort::Bool));
    }
    // (1) Params first: their terms exist before any op reads them.
    for &pv in &func.params {
        declare_value(p, func, pv, &mut enc);
    }
    // (2) Declare every value an instruction reads or writes, then pin
    // havoc'd branch conditions to a stable `hc<b>` const — the single
    // source of truth guards and phi both read. Splitting declaration out
    // of `encode_ops` lets guards (which need Branch conds declared) run
    // before op defining-equalities (which need edge guards for phi).
    declare_pass(p, func, &mut enc);
    // (3) Guard structure over the cut DAG.
    encode_guards(func, &reachable, &mut enc)?;
    // (4) Defining equalities for modeled ops (Assign/BinOp/UnOp/Phi/Make);
    // everything else keeps its declared-but-unconstrained (havoc) dst.
    encode_ops(p, func, &mut enc);
    encode_load_forwarding(func, &mut enc);
    if enc.asserts.len() > ASSERT_CAP {
        return Err(format!(
            "{}: encoding exceeds {ASSERT_CAP} assertions; skipped",
            p.func_name(f)
        ));
    }
    Ok(enc)
}

/// Introduce `v`'s term if its type is modelable: declare the const and,
/// for Seq-sorted values, assert 0 <= len <= cap. Constants get defining
/// equalities. Idempotent.
fn declare_value(p: &Program, func: &Function, v: ValueId, enc: &mut EncodedFunc) {
    if enc.values.contains_key(&v) {
        return;
    }
    let info = func.value(v);
    let Some(sort) = sort_of(p.types(), info.ty) else {
        return;
    };
    let name = value_name(func, v);
    let t = Term::var(&name, sort.clone());
    enc.consts.push((name, sort.clone()));
    // The invariant is skipped ONLY for an `Op::Slice` dst, not a
    // `Make{Slice}` one — the two cases are NOT symmetric (review
    // finding, Task 8 fast-follow):
    //
    // - `Make{Slice}`'s len/cap come directly from `make`'s own args,
    //   and `len<=cap` there is exactly `make`'s own runtime
    //   precondition: Go's `make([]T, m, n)` panics whenever m>n, so NO
    //   reachable execution continues past it with m>n — asserting the
    //   invariant on a `Make{Slice}` dst renders that (correctly) as
    //   Unsat and masks nothing real.
    // - `Op::Slice`'s new len/cap are a raw `high-low`/`cap-low`
    //   arithmetic derivation that CAN legitimately disagree with the
    //   invariant on an out-of-bounds slice — that disagreement is
    //   exactly the bug bounds-checking needs to keep Sat, not an
    //   impossible execution to render Unsat. Forcing the invariant
    //   there makes the WHOLE function's assertion set inconsistent,
    //   silently swallowing every finding in it (the original Task 8
    //   corpus finding, on a `Slice` dst specifically — not `Make`).
    if sort == seq_datatype().sort() && !is_slice_derived(func, v) {
        seq_invariant(&t, enc);
    }
    if let ValueKind::Const(c) = &info.kind
        && let Some(lit) = const_term(p, info.ty, c)
        && let Ok(eq) = Term::eq(t.clone(), lit)
    {
        enc.asserts.push(eq);
    }
    enc.values.insert(v, t);
}

/// True iff `v` is the dst of an `Op::Slice` (NOT `Make{Slice}` — see
/// the invariant-skip comment at its call site, `declare_value`)
/// somewhere in `func`. Linear in `func`'s instruction count; functions
/// are bounded by `ASSERT_CAP` so this stays cheap.
fn is_slice_derived(func: &Function, v: ValueId) -> bool {
    func.blocks
        .iter()
        .flat_map(|b| &b.instrs)
        .any(|ins| matches!(&ins.op, Op::Slice { dst, .. } if *dst == v))
}

/// 0 <= seq-len(t) <= seq-cap(t) (unsigned): every Seq value the
/// encoder introduces carries the Go invariant, so cap-based slice
/// bounds can never be tighter than len-based ones.
fn seq_invariant(t: &Term, enc: &mut EncodedFunc) {
    let dt = seq_datatype();
    let (Ok(len), Ok(cap)) = (
        Term::dt_get(&dt, "seq-val", "seq-len", t.clone()),
        Term::dt_get(&dt, "seq-val", "seq-cap", t.clone()),
    ) else {
        return;
    };
    if let Ok(inv) = Term::bv_cmp(BvCmpOp::Ule, len, cap) {
        enc.asserts.push(inv);
    }
}

fn const_term(p: &Program, ty: TypeId, c: &ConstVal) -> Option<Term> {
    match c {
        ConstVal::Bool(b) => Some(Term::bool_lit(*b)),
        ConstVal::Int(i) => {
            let (w, _) = int_repr(p.types(), ty)?;
            // two's-complement truncation into the width
            let masked = (*i as u128) & (u128::MAX >> (128 - w));
            Some(Term::bv_lit(w, masked))
        }
        ConstVal::Nil => match sort_of(p.types(), ty)? {
            Sort::Datatype(n) if n == "Ptr" => Some(ptr_nil()),
            s if s == seq_datatype().sort() => seq_lit(0, 0),
            _ => None,
        },
        ConstVal::Str(bytes) => seq_lit(bytes.len() as u64, bytes.len() as u64),
        _ => None, // BigInt/Float/Complex/Opaque havoc
    }
}

fn seq_lit(len: u64, cap: u64) -> Option<Term> {
    Term::dt_ctor(
        &seq_datatype(),
        "seq-val",
        vec![Term::bv_lit(64, len as u128), Term::bv_lit(64, cap as u128)],
    )
    .ok()
}

/// Guard structure over the cut DAG: g0 = true; for every other block,
/// g_b = OR of incoming edge guards; a Branch edge conjoins the branch
/// condition (positional: succs[0] = cond, succs[1] = ¬cond). A block
/// whose every in-edge was cut is unreachable in the DAG: g_b = false.
/// `reachable[b]` is the DFS reachability flag (`cut_back_edges_visited`):
/// a block NOT reachable from the entry is pinned `g_b = false` outright,
/// rather than built from its incoming-edge OR — an unreachable cycle's
/// edges survive the back-edge cut (the DFS never traverses them), so
/// their mutually-supporting equations would otherwise stay satisfiable
/// with the guards true and let `reach_query` mint a false positive.
fn encode_guards(func: &Function, reachable: &[bool], enc: &mut EncodedFunc) -> Result<(), String> {
    let n = func.blocks.len();
    // Incoming edge guards per block, over the cut DAG only.
    let mut incoming: Vec<Vec<Term>> = vec![Vec::new(); n];
    for b in 0..n {
        for &s in &enc.dag_succs[b] {
            let s = s as usize;
            if s >= n {
                continue;
            }
            if let Some(edge) = edge_guard(func, enc, b, s) {
                incoming[s].push(edge);
            }
        }
    }
    for (b, edges) in incoming.into_iter().enumerate() {
        let gb = enc.guards[b].clone();
        let reachable_b = reachable.get(b).copied().unwrap_or(false);
        let rhs = if b == 0 {
            Term::bool_lit(true)
        } else if !reachable_b || edges.is_empty() {
            // Either the block is unreachable from the entry, or every
            // in-edge was cut in the DAG — it can't be reached, so
            // g_b = false. The reachability arm is what closes the
            // crafted-.gvir hole: an unreachable CYCLE keeps its edges (so
            // `edges` is non-empty) yet its guard equations are mutually
            // satisfiable as true unless forced false here.
            Term::bool_lit(false)
        } else if edges.len() == 1 {
            edges.into_iter().next().expect("len == 1")
        } else {
            Term::or(edges).map_err(|e| format!("guard or: {e}"))?
        };
        enc.asserts
            .push(Term::eq(gb, rhs).map_err(|e| format!("guard eq: {e}"))?);
    }
    Ok(())
}

/// The guard on edge `from -> to`: the source block's guard, conjoined
/// with the branch condition when `from` ends in a two-way Branch
/// (positional: succs[0] = cond, succs[1] = ¬cond). SINGLE source of
/// truth — phi (`op_def`) reuses this so its ite conditions can never
/// diverge from the guard structure. Read-only: any havoc'd condition
/// was already pinned to `hc<from>` by `declare_pass`.
fn edge_guard(func: &Function, enc: &EncodedFunc, from: usize, to: usize) -> Option<Term> {
    let gb = enc.guards.get(from)?.clone();
    // Positional index within the ORIGINAL succs decides branch polarity
    // (dag_succs preserves order).
    let orig_pos = func
        .blocks
        .get(from)?
        .succs
        .iter()
        .position(|&x| x as usize == to);
    match (branch_cond(func, enc, from), orig_pos) {
        (Some(c), Some(0)) => Term::and(vec![gb, c]).ok(),
        (Some(c), Some(1)) => Term::not(c)
            .ok()
            .and_then(|nc| Term::and(vec![gb, nc]).ok()),
        _ => Some(gb),
    }
}

/// The branch condition term for block `b` when it ends in a Branch: the
/// modeled operand term, else the `hc<b>` havoc const `declare_pass`
/// pinned (missing info = nondeterminism, both branches stay possible).
/// `None` when `b` does not end in a Branch.
fn branch_cond(func: &Function, enc: &EncodedFunc, b: usize) -> Option<Term> {
    match &func.blocks.get(b)?.instrs.last()?.op {
        Op::Branch { cond } => Some(
            enc.values
                .get(cond)
                .cloned()
                .unwrap_or_else(|| Term::var(&format!("hc{b}"), Sort::Bool)),
        ),
        _ => None,
    }
}

/// Declare every value an instruction reads or writes (making havoc
/// sound: even an unmodeled op's dst gets a typed const), then register a
/// stable `hc<b>` const for each Branch whose condition stays havoc'd, so
/// guards and phi reference the SAME symbol instead of each minting one.
fn declare_pass(p: &Program, func: &Function, enc: &mut EncodedFunc) {
    for block in &func.blocks {
        for ins in &block.instrs {
            for v in op_values(&ins.op) {
                declare_value(p, func, v, enc);
            }
        }
    }
    for (b, block) in func.blocks.iter().enumerate() {
        if let Some(Op::Branch { cond }) = block.instrs.last().map(|i| &i.op)
            && !enc.values.contains_key(cond)
        {
            enc.consts.push((format!("hc{b}"), Sort::Bool));
        }
    }
}

/// Every value id an op reads or writes (dsts included; `Option` dsts and
/// operands flattened). Mechanical — the declare pass runs this over every
/// instruction so unmodeled writes still get a declared const.
fn op_values(op: &Op) -> Vec<ValueId> {
    let mut vs = Vec::new();
    match op {
        Op::Assign { dst, src }
        | Op::Convert { dst, src }
        | Op::MakeInterface { dst, src }
        | Op::TypeAssert { dst, src, .. } => vs.extend([*dst, *src]),
        Op::Alloc { dst, .. } => vs.push(*dst),
        Op::Load { dst, addr } => vs.extend([*dst, *addr]),
        Op::Store { addr, val } => vs.extend([*addr, *val]),
        Op::FieldAddr { dst, base, .. } | Op::Field { dst, base, .. } => vs.extend([*dst, *base]),
        Op::IndexAddr { dst, base, index } | Op::Index { dst, base, index } => {
            vs.extend([*dst, *base, *index])
        }
        Op::Lookup { dst, map, key, .. } => vs.extend([*dst, *map, *key]),
        Op::Slice {
            dst,
            base,
            low,
            high,
            max,
        } => {
            vs.extend([*dst, *base]);
            vs.extend(low.iter().chain(high.iter()).chain(max.iter()).copied());
        }
        Op::BinOp { dst, lhs, rhs, .. } => vs.extend([*dst, *lhs, *rhs]),
        Op::UnOp { dst, operand, .. } => vs.extend([*dst, *operand]),
        Op::Extract { dst, tuple, .. } => vs.extend([*dst, *tuple]),
        Op::Phi { dst, edges } => {
            vs.push(*dst);
            vs.extend(edges.iter().copied());
        }
        Op::Call { dst, args, .. } => {
            vs.extend(dst.iter().copied());
            vs.extend(args.iter().copied());
        }
        Op::MakeClosure { dst, bindings, .. } => {
            vs.push(*dst);
            vs.extend(bindings.iter().copied());
        }
        Op::Make { dst, args, .. } => {
            vs.push(*dst);
            vs.extend(args.iter().copied());
        }
        Op::Send { chan, val } => vs.extend([*chan, *val]),
        Op::Recv { dst, chan, .. } => vs.extend([*dst, *chan]),
        Op::CloseChan { chan } => vs.push(*chan),
        Op::Select { dst, arms, .. } => {
            vs.push(*dst);
            for a in arms {
                vs.push(a.chan);
                vs.extend(a.send.iter().copied());
            }
        }
        Op::Go { args, .. } | Op::Defer { args, .. } => vs.extend(args.iter().copied()),
        Op::Return { vals } => vs.extend(vals.iter().copied()),
        Op::Branch { cond } => vs.push(*cond),
        Op::Panic { val } => vs.push(*val),
        Op::Lock { mu, .. } => vs.push(*mu),
        Op::Havoc { dst } => vs.extend(dst.iter().copied()),
        Op::Jump => {}
    }
    vs
}

/// Assert the defining equality for each modeled op; unmodeled ops leave
/// their dsts declared but unconstrained (havoc).
fn encode_ops(p: &Program, func: &Function, enc: &mut EncodedFunc) {
    for (bi, block) in func.blocks.iter().enumerate() {
        for ins in &block.instrs {
            if let Some(def) = op_def(p, func, bi, &ins.op, enc) {
                enc.asserts.push(def);
            }
        }
    }
}

/// Structural value number for an address: Assign-transparent;
/// FieldAddr keyed by (base key, field); IndexAddr by (base key,
/// index value). Everything else is its own root. Two addresses with
/// equal keys compute the same location in SSA (address ops are pure).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum AddrKey {
    Root(ValueId),
    Field(Box<AddrKey>, u32),
    Index(Box<AddrKey>, ValueId),
}

fn key_of(keys: &BTreeMap<ValueId, AddrKey>, v: ValueId) -> AddrKey {
    keys.get(&v).cloned().unwrap_or(AddrKey::Root(v))
}

/// DFS postorder from the entry over the cut DAG, reversed — a
/// topological order (the DAG is acyclic by construction). Only blocks
/// reachable from the entry appear; unreachable blocks have guards
/// pinned false, nothing to forward.
pub(crate) fn topo_order(dag: &[Vec<u32>]) -> Vec<usize> {
    let n = dag.len();
    let mut order = Vec::with_capacity(n);
    if n == 0 {
        return order;
    }
    let mut state = vec![0u8; n];
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    state[0] = 1;
    while let Some(frame) = stack.last_mut() {
        let (b, i) = (frame.0, frame.1);
        if i < dag[b].len() {
            frame.1 += 1;
            let s = dag[b][i] as usize;
            if s < n && state[s] == 0 {
                state[s] = 1;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
            stack.pop();
        }
    }
    order.reverse();
    order
}

/// Same-function load forwarding (fix-wave fix 2a): repeated loads of
/// the same value-numbered address see the same value when no
/// potentially-aliasing write can intervene. Blocks are walked in
/// topological order, so any Store on any path between two loads sits
/// between them in walk order (topo order respects edges; a parallel
/// branch's Store only costs precision, never soundness). A Store or a
/// dst-less Havoc (unmodeled op with unknown effect) conservatively
/// clears ALL pending forwards. Calls deliberately do NOT invalidate:
/// assuming callees don't mutate a re-read field is a documented
/// under-approximation (threat model, "deliberate under-
/// approximations") — the bug-finder invariant prefers a missed exotic
/// mutation to the shakeout's dominant FP class (mechanism 1).
fn encode_load_forwarding(func: &Function, enc: &mut EncodedFunc) {
    let mut keys: BTreeMap<ValueId, AddrKey> = BTreeMap::new();
    let mut seen: BTreeMap<AddrKey, ValueId> = BTreeMap::new();
    for b in topo_order(&enc.dag_succs) {
        let Some(block) = func.blocks.get(b) else {
            continue;
        };
        for ins in &block.instrs {
            match &ins.op {
                Op::Assign { dst, src } => {
                    if let Some(k) = keys.get(src).cloned() {
                        keys.insert(*dst, k);
                    }
                }
                Op::FieldAddr { dst, base, field } => {
                    let bk = key_of(&keys, *base);
                    keys.insert(*dst, AddrKey::Field(Box::new(bk), *field));
                }
                Op::IndexAddr { dst, base, index } => {
                    let bk = key_of(&keys, *base);
                    keys.insert(*dst, AddrKey::Index(Box::new(bk), *index));
                }
                Op::Load { dst, addr } => {
                    let k = key_of(&keys, *addr);
                    if let Some(prev) = seen.get(&k) {
                        if let (Some(d), Some(pv)) =
                            (enc.values.get(dst).cloned(), enc.values.get(prev).cloned())
                            && let Ok(eq) = Term::eq(d, pv)
                        {
                            enc.asserts.push(eq);
                        }
                    } else {
                        seen.insert(k, *dst);
                    }
                }
                Op::Store { .. } | Op::Havoc { dst: None } => seen.clear(),
                _ => {}
            }
        }
    }
}

/// True iff `v`'s value is minted from uintptr arithmetic: v is
/// uintptr-typed, or unsafe.Pointer-typed ("Pointer" is go/types' Basic
/// name for it — no other Basic collides) and itself the dst of a
/// Convert from a uintptr-provenance value. `visited` memoizes explored
/// ValueIds (review fix): the depth cap alone bounds PATH length, not
/// TOTAL work — legitimate SSA defines each register once, but lowering
/// doesn't enforce dst uniqueness on crafted `.gvir`, so N instructions
/// all `Convert { dst: v, src: v }` would otherwise make every level's
/// `.any()` match all N and recurse without short-circuiting (~N^depth
/// calls, each rescanning every instruction). Marking `v` visited before
/// recursing bounds total work by the function's own value count instead
/// — a crafted fan-out/self-loop degrades to "no" after re-visiting each
/// id at most once, never hangs (`.gvir` bytes are untrusted; parsers of
/// bytes the analyzer didn't write reject, never panic). `BTreeSet`, not
/// `HashSet`: consistent with this codebase's determinism convention,
/// even though iteration order can't reach output here (membership only).
fn uintptr_provenance(
    p: &Program,
    func: &Function,
    v: ValueId,
    depth: u32,
    visited: &mut BTreeSet<ValueId>,
) -> bool {
    if depth > 8 || !visited.insert(v) {
        return false;
    }
    match basic_name(p.types(), func.value(v).ty) {
        Some("uintptr") => true,
        Some("Pointer") => func.blocks.iter().flat_map(|b| &b.instrs).any(|ins| {
            matches!(&ins.op, Op::Convert { dst, src } if *dst == v
                && uintptr_provenance(p, func, *src, depth + 1, visited))
        }),
        _ => false,
    }
}

/// The defining equality for a modeled op, if every term it needs exists;
/// `None` = havoc (dst stays declared but unconstrained). Never panics:
/// missing terms and out-of-range preds degrade to `None`.
fn op_def(p: &Program, func: &Function, block: usize, op: &Op, enc: &EncodedFunc) -> Option<Term> {
    let t = |v: &ValueId| enc.values.get(v).cloned();
    match op {
        Op::Assign { dst, src } => Term::eq(t(dst)?, t(src)?).ok(),
        Op::BinOp {
            dst,
            kind,
            lhs,
            rhs,
        } => {
            let d = t(dst)?;
            let rhs_term = binop_term(p, func, *kind, lhs, t(lhs)?, t(rhs)?)?;
            Term::eq(d, rhs_term).ok()
        }
        Op::UnOp { dst, kind, operand } => {
            let d = t(dst)?;
            let o = t(operand)?;
            let rhs = match kind {
                UnOpKind::Not => Term::not(o).ok()?,
                UnOpKind::Neg => {
                    // -x = 0 - x at the operand's width.
                    let (w, _) = int_repr(p.types(), func.value(*operand).ty)?;
                    Term::bv_bin(BvBinOp::Sub, Term::bv_lit(w, 0), o).ok()?
                }
                UnOpKind::BitNot => {
                    // ^x = x xor all-ones at the operand's width.
                    let (w, _) = int_repr(p.types(), func.value(*operand).ty)?;
                    let ones = u128::MAX >> (128 - w);
                    Term::bv_bin(BvBinOp::Xor, o, Term::bv_lit(w, ones)).ok()?
                }
            };
            Term::eq(d, rhs).ok()
        }
        Op::Phi { dst, edges } => {
            let d = t(dst)?;
            let preds = &func.blocks.get(block)?.preds;
            // Keep only edges whose pred edge survives in the cut DAG;
            // `Op::Phi` operand i pairs with preds[i].
            let mut kept: Vec<(Term, Term)> = Vec::new(); // (edge guard, value)
            for (i, ev) in edges.iter().enumerate() {
                let pr = *preds.get(i)? as usize;
                if pr >= func.blocks.len() {
                    return None;
                }
                if !enc.dag_succs[pr].contains(&(block as u32)) {
                    continue; // back edge: cut
                }
                kept.push((edge_guard(func, enc, pr, block)?, t(ev)?));
            }
            let (_last_g, last_v) = kept.pop()?;
            // The final (else) branch needs no test.
            let mut acc = last_v;
            for (g, v) in kept.into_iter().rev() {
                acc = Term::ite(g, v, acc).ok()?;
            }
            Term::eq(d, acc).ok()
        }
        Op::Make {
            dst,
            kind: MakeKind::Slice,
            args,
        } => {
            let d = t(dst)?;
            let len = args.first().and_then(t)?;
            let cap = args.get(1).and_then(t).unwrap_or_else(|| len.clone());
            let ctor = Term::dt_ctor(&seq_datatype(), "seq-val", vec![len, cap]).ok()?;
            Term::eq(d, ctor).ok()
        }
        // `s[low:high(:max)]`: new len = high-low, new cap = (max or the
        // base's own cap) - low (Go spec §"Slice expressions") — Task 8
        // addition: real `make([]T, n)` with a constant `n` lowers to
        // `Alloc` + a whole-array `Slice` (never `MakeSlice`), so leaving
        // Slice unmodeled havocs the len/cap of essentially every slice a
        // corpus actually produces, defeating bounds-checking entirely.
        Op::Slice {
            dst,
            base,
            low,
            high,
            max,
        } => {
            let d = t(dst)?;
            let (base_len, base_cap) = match array_len(p.types(), func.value(*base).ty) {
                Some(n) => {
                    let lit = Term::bv_lit(64, n as u128);
                    (lit.clone(), lit)
                }
                None => {
                    let b = enc.values.get(base)?.clone();
                    if b.sort() != &seq_datatype().sort() {
                        return None;
                    }
                    (
                        Term::dt_get(&seq_datatype(), "seq-val", "seq-len", b.clone()).ok()?,
                        Term::dt_get(&seq_datatype(), "seq-val", "seq-cap", b).ok()?,
                    )
                }
            };
            let low_t = match low {
                Some(l) => t(l)?,
                None => Term::bv_lit(64, 0),
            };
            let high_t = match high {
                Some(h) => t(h)?,
                None => base_len,
            };
            let cap_src = match max {
                Some(m) => t(m)?,
                None => base_cap,
            };
            let new_len = Term::bv_bin(BvBinOp::Sub, high_t, low_t.clone()).ok()?;
            let new_cap = Term::bv_bin(BvBinOp::Sub, cap_src, low_t).ok()?;
            let ctor = Term::dt_ctor(&seq_datatype(), "seq-val", vec![new_len, new_cap]).ok()?;
            Term::eq(d, ctor).ok()
        }
        // The `len` builtin ties its dst to the base's own seq-len
        // accessor rather than havoc'ing (Task 8): without this, a
        // `len(s)`-guarded index/slice can never be proven safe, since
        // the guard's condition would be about a value wholly
        // disconnected from `s`'s real length.
        Op::Call {
            dst: Some(d),
            callee: Callee::Builtin(name),
            args,
        } if name == "len" => {
            let dt = t(d)?;
            let arg = args.first().and_then(|a| enc.values.get(a).cloned())?;
            if arg.sort() != &seq_datatype().sort() {
                return None;
            }
            let len = Term::dt_get(&seq_datatype(), "seq-val", "seq-len", arg).ok()?;
            Term::eq(dt, len).ok()
        }
        // A pointer minted from uintptr arithmetic is never nil (fix-wave
        // fix 3): unsafe.Pointer(uintptr(base)+off) idioms compute offsets
        // from live bases, and producing exactly nil would need deliberate
        // 64-bit wraparound — documented under-approximation (threat model,
        // "deliberate under-approximations"). A plain pointer→unsafe.Pointer
        // →pointer pun keeps its nilability: its provenance is a pointer,
        // not uintptr.
        Op::Convert { dst, src } => {
            let d = t(dst)?;
            if d.sort() != &ptr_sort()
                || !uintptr_provenance(p, func, *src, 0, &mut BTreeSet::new())
            {
                return None;
            }
            Term::not(ptr_is_nil(d).ok()?).ok()
        }
        // Address-of ops never produce nil (fix-wave fix 1): a Go
        // allocation, field address, or element address is a valid non-nil
        // address — the op faults on a bad base before a value exists, so
        // any continuing execution holds a non-nil dst. The base's own
        // nilability stays a separate obligation at its own site.
        Op::Alloc { dst, .. } | Op::FieldAddr { dst, .. } | Op::IndexAddr { dst, .. } => {
            let d = t(dst)?;
            if d.sort() != &ptr_sort() {
                return None;
            }
            Term::not(ptr_is_nil(d).ok()?).ok()
        }
        _ => None, // Load/Store/Call/... havoc (declared, unconstrained); Convert havocs
                   // except the uintptr-provenance arm above
    }
}

/// The RHS term for a modeled BinOp. Signedness (and the mask width for
/// `&^`) come from the OPERAND type (`lhs`), never the dst: a comparison's
/// dst is Bool but the signed/unsigned choice is the compared ints'.
/// `None` for ops with no bitvector model (string concat `+`, float
/// arithmetic — operands lack `int_repr` and aren't Eq/Neq) → havoc.
fn binop_term(
    p: &Program,
    func: &Function,
    kind: BinOpKind,
    lhs: &ValueId,
    l: Term,
    r: Term,
) -> Option<Term> {
    use BinOpKind as K;
    // Eq/Neq are defined for every sort (Ptr/Seq/Bool/BitVec).
    match kind {
        K::Eq => return Term::eq(l, r).ok(),
        K::Neq => return Term::not(Term::eq(l, r).ok()?).ok(),
        _ => {}
    }
    // Every remaining op is bitvector-only.
    let (w, signed) = int_repr(p.types(), func.value(*lhs).ty)?;
    match kind {
        K::Add => Term::bv_bin(BvBinOp::Add, l, r).ok(),
        K::Sub => Term::bv_bin(BvBinOp::Sub, l, r).ok(),
        K::Mul => Term::bv_bin(BvBinOp::Mul, l, r).ok(),
        K::And => Term::bv_bin(BvBinOp::And, l, r).ok(),
        K::Or => Term::bv_bin(BvBinOp::Or, l, r).ok(),
        K::Xor => Term::bv_bin(BvBinOp::Xor, l, r).ok(),
        K::Shl => Term::bv_bin(BvBinOp::Shl, l, r).ok(),
        K::Shr => Term::bv_bin(if signed { BvBinOp::Ashr } else { BvBinOp::Lshr }, l, r).ok(),
        K::Div => Term::bv_bin(if signed { BvBinOp::Sdiv } else { BvBinOp::Udiv }, l, r).ok(),
        K::Rem => Term::bv_bin(if signed { BvBinOp::Srem } else { BvBinOp::Urem }, l, r).ok(),
        K::AndNot => {
            // x &^ y = x & ~y ; ~y = y xor all-ones.
            let ones = u128::MAX >> (128 - w);
            let noty = Term::bv_bin(BvBinOp::Xor, r, Term::bv_lit(w, ones)).ok()?;
            Term::bv_bin(BvBinOp::And, l, noty).ok()
        }
        K::Lt => Term::bv_cmp(if signed { BvCmpOp::Slt } else { BvCmpOp::Ult }, l, r).ok(),
        K::Leq => Term::bv_cmp(if signed { BvCmpOp::Sle } else { BvCmpOp::Ule }, l, r).ok(),
        // x > y  ⇒  y < x ; x >= y  ⇒  y <= x (swap operands).
        K::Gt => Term::bv_cmp(if signed { BvCmpOp::Slt } else { BvCmpOp::Ult }, r, l).ok(),
        K::Geq => Term::bv_cmp(if signed { BvCmpOp::Sle } else { BvCmpOp::Ule }, r, l).ok(),
        K::Eq | K::Neq => None, // handled above
    }
}

// ---- Task 10: trace reconstruction from sat models -------------------
//
// Everything below reads a Sat model's TEXT for DISPLAY purposes only —
// verdicts never depend on it (Sat/Unsat/Unknown comes from the solver's
// own result code, computed before any of this runs). Two backend
// formats exist: Z3Native's `Z3_model_to_string` renders "name -> value"
// lines; a CLI/process backend's `(get-model)` response is an s-expr
// list of `(define-fun name () Sort value)` forms. Both are wrapped so
// any parse irregularity — malformed text, an unexpected shape — yields
// an empty (or partial) map rather than a panic or a fabricated entry.

/// Best-effort model reading for DISPLAY ONLY (verdicts never depend on
/// it): "gN -> true" lines (Z3Native / Z3_model_to_string) and
/// "(define-fun gN () Bool true)" s-exprs (get-model backends).
pub fn guard_values(model_text: &str) -> BTreeMap<String, bool> {
    model_entries(model_text)
        .into_iter()
        .filter_map(|(name, value)| match value.as_str() {
            "true" => Some((name, true)),
            "false" => Some((name, false)),
            _ => None, // non-bool: ignored (display plumbing only)
        })
        .collect()
}

/// Param/variable bindings as display strings ("p0" -> "(ptr-nil)").
/// Sanitization happens at render time (Task 11), not here.
pub fn model_bindings(model_text: &str) -> BTreeMap<String, String> {
    model_entries(model_text)
}

/// Shared model-text parsing: the arrow line format is tried first;
/// when no arrow line is found at all, fall back to
/// `goverify_solver::parse_sexpr` over the WHOLE text, walking
/// `define-fun` triples. Never panics — any parse error yields the
/// empty map.
fn model_entries(model_text: &str) -> BTreeMap<String, String> {
    let arrow = arrow_entries(model_text);
    if !arrow.is_empty() {
        return arrow;
    }
    define_fun_entries(model_text).unwrap_or_default()
}

/// "name -> value" lines (Z3Native's `Z3_model_to_string` format).
/// Blank/malformed lines are skipped, not fatal.
fn arrow_entries(model_text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in model_text.lines() {
        let Some((name, value)) = line.split_once("->") else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || value.is_empty() {
            continue;
        }
        out.insert(name.to_string(), value.to_string());
    }
    out
}

/// `(define-fun name () Sort value)` triples from a `(get-model)`
/// response, keyed by name with `value`'s own source text as the
/// display string. `None` on any parse failure (garbage input).
fn define_fun_entries(model_text: &str) -> Option<BTreeMap<String, String>> {
    let (top, _) = goverify_solver::parse_sexpr(model_text).ok()?;
    let mut out = BTreeMap::new();
    collect_define_funs(&top, &mut out);
    Some(out)
}

fn collect_define_funs(e: &goverify_solver::SExpr, out: &mut BTreeMap<String, String>) {
    use goverify_solver::SExpr;
    let SExpr::List(items) = e else {
        return;
    };
    if let [
        SExpr::Atom(head),
        SExpr::Atom(name),
        SExpr::List(_args),
        _sort,
        body,
    ] = items.as_slice()
        && head == "define-fun"
    {
        out.insert(name.clone(), sexpr_text(body));
        return;
    }
    for item in items {
        collect_define_funs(item, out);
    }
}

/// Reconstruct a sub-expression's own source text (there's no byte-range
/// tracking through `parse_sexpr`, so this re-renders from the parsed
/// tree instead — stable and good enough for a display string).
fn sexpr_text(e: &goverify_solver::SExpr) -> String {
    match e {
        goverify_solver::SExpr::Atom(s) => s.clone(),
        goverify_solver::SExpr::List(items) => {
            format!(
                "({})",
                items.iter().map(sexpr_text).collect::<Vec<_>>().join(" ")
            )
        }
    }
}

/// Entry-to-somewhere path: from block 0, repeatedly take the first DAG
/// successor whose guard is true in the model (unassigned = false);
/// stops when no successor qualifies. Deterministic; empty on any
/// irregularity (missing g0, malformed model).
pub fn violating_path(
    func: &Function,
    dag_succs: &[Vec<u32>],
    guards: &BTreeMap<String, bool>,
) -> Vec<u32> {
    // A block-0 seed is only sound when block 0 exists: `trace_for`
    // indexes `blocks[0]` on the returned path, so an empty function must
    // yield an empty path (unreachable via real checkers — go/ssa
    // functions always have an entry block — but crafted/degraded .gvir
    // need not).
    if func.blocks.is_empty() {
        return Vec::new();
    }
    if guards.get("g0") != Some(&true) {
        return Vec::new();
    }
    let mut path = vec![0u32];
    let mut cur = 0usize;
    // clippy (while_let_loop): the brief's `loop { let-else { break }; ... }`
    // shape has no code path after the let-else other than falling through
    // to the body, so it's exactly a `while let` — rewritten for lint
    // cleanliness, semantics unchanged.
    while let Some(next) = dag_succs.get(cur).and_then(|ss| {
        ss.iter()
            .find(|&&s| guards.get(&format!("g{s}")) == Some(&true))
    }) {
        let next = *next as usize;
        if next >= func.blocks.len() || path.len() > func.blocks.len() {
            break; // safety: DAG walk can't exceed block count
        }
        path.push(next as u32);
        cur = next;
    }
    path
}

#[cfg(test)]
mod tests {
    use goverify_ir::Program;
    use goverify_solver::Sort;

    use goverify_ir::FuncId;

    use super::*;
    use crate::testpkg::{block, block_p, func, instr, pkg};

    fn types_program() -> Program {
        // Interns basic types via a tiny package. testpkg's `pkg` builder
        // gives us a TypeTable; intern reprs directly through gvir types.
        use goverify_extract::gvir;
        let package = gvir::Package {
            import_path: "t".into(),
            types: vec![
                gvir::Type {
                    id: 1,
                    repr: "int".into(),
                    kind: gvir::TypeKind::Basic as i32,
                    name: "int".into(),
                    ..Default::default()
                },
                gvir::Type {
                    id: 2,
                    repr: "uint8".into(),
                    kind: gvir::TypeKind::Basic as i32,
                    name: "uint8".into(),
                    ..Default::default()
                },
                gvir::Type {
                    id: 3,
                    repr: "bool".into(),
                    kind: gvir::TypeKind::Basic as i32,
                    name: "bool".into(),
                    ..Default::default()
                },
                gvir::Type {
                    id: 4,
                    repr: "T".into(),
                    kind: gvir::TypeKind::Struct as i32,
                    ..Default::default()
                },
                gvir::Type {
                    id: 5,
                    repr: "*T".into(),
                    kind: gvir::TypeKind::Pointer as i32,
                    elem: 4,
                    ..Default::default()
                },
                gvir::Type {
                    id: 6,
                    repr: "[]int".into(),
                    kind: gvir::TypeKind::Slice as i32,
                    elem: 1,
                    ..Default::default()
                },
                gvir::Type {
                    id: 7,
                    repr: "string".into(),
                    kind: gvir::TypeKind::Basic as i32,
                    name: "string".into(),
                    ..Default::default()
                },
                gvir::Type {
                    id: 8,
                    repr: "float64".into(),
                    kind: gvir::TypeKind::Basic as i32,
                    name: "float64".into(),
                    ..Default::default()
                },
                gvir::Type {
                    id: 9,
                    repr: "MyInt".into(),
                    kind: gvir::TypeKind::Named as i32,
                    name: "MyInt".into(),
                    elem: 1,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        Program::from_packages(vec![package])
    }

    fn lookup(p: &Program, repr: &str) -> goverify_ir::TypeId {
        // TypeTable interns by repr; walk ids until repr matches.
        let types = p.types();
        (0..u32::MAX)
            .map(goverify_ir::TypeId)
            .take_while(|&t| !types.repr(t).is_empty() || t.0 < 64)
            .find(|&t| types.repr(t) == repr)
            .expect("type interned")
    }

    #[test]
    fn sort_of_maps_the_v1_theory_surface() {
        let p = types_program();
        let ty = p.types();
        assert_eq!(sort_of(ty, lookup(&p, "int")), Some(Sort::BitVec(64)));
        assert_eq!(sort_of(ty, lookup(&p, "uint8")), Some(Sort::BitVec(8)));
        assert_eq!(sort_of(ty, lookup(&p, "bool")), Some(Sort::Bool));
        assert_eq!(
            sort_of(ty, lookup(&p, "*T")),
            Some(goverify_solver::ptr_sort())
        );
        assert_eq!(
            sort_of(ty, lookup(&p, "[]int")),
            Some(seq_datatype().sort())
        );
        assert_eq!(
            sort_of(ty, lookup(&p, "string")),
            Some(seq_datatype().sort())
        );
        assert_eq!(
            sort_of(ty, lookup(&p, "float64")),
            None,
            "floats havoc in v1"
        );
        assert_eq!(
            sort_of(ty, lookup(&p, "MyInt")),
            Some(Sort::BitVec(64)),
            "named -> underlying"
        );
    }

    #[test]
    fn int_repr_distinguishes_width_and_sign() {
        let p = types_program();
        let ty = p.types();
        assert_eq!(int_repr(ty, lookup(&p, "int")), Some((64, true)));
        assert_eq!(int_repr(ty, lookup(&p, "uint8")), Some((8, false)));
        assert_eq!(int_repr(ty, lookup(&p, "bool")), None);
        assert_eq!(
            int_repr(ty, lookup(&p, "float64")),
            None,
            "I1 lesson: floats are not ints"
        );
    }

    #[test]
    fn cut_back_edges_yields_a_dag_keeping_forward_edges() {
        // 0 -> 1, 1 -> {2, 1(self)}, 2 -> 0 (loop): both cycle edges cut.
        let f = func(
            "t.F",
            vec![
                block(0, vec![instr("Jump")], vec![1]),
                block(1, vec![instr("If")], vec![2, 1]),
                block(2, vec![instr("Jump")], vec![0]),
            ],
        );
        let p = Program::from_packages(vec![pkg("t", vec![f])]);
        let fid = p.lookup_func("t.F").unwrap();
        let dag = cut_back_edges(p.func(fid).unwrap());
        assert_eq!(dag[0], vec![1]);
        assert_eq!(dag[1], vec![2], "self-edge 1->1 cut");
        assert!(dag[2].is_empty(), "2->0 back edge cut");
    }

    fn one_func_program(f: goverify_extract::gvir::Function) -> (Program, FuncId) {
        let p = Program::from_packages(vec![pkg("t", vec![f])]);
        let id = p.lookup_func("t.F").unwrap();
        (p, id)
    }

    #[test]
    fn unreachable_cycle_guards_are_forced_false() {
        // 0 returns (reachable); 1 -> 2 -> 1 is an unreachable self-cycle.
        // The DFS from block 0 never touches 1/2, so their edges survive
        // the back-edge cut — without the reachability override their
        // guard equations (g1 depends on g2, g2 on g1) stay mutually
        // satisfiable as true, so reach_query(1) would return Sat and mint
        // a crafted-.gvir false positive. Every unreachable block must be
        // pinned g = false instead.
        let f = func(
            "t.F",
            vec![
                block_p(0, vec![instr("Return")], vec![], vec![]),
                block_p(1, vec![instr("Jump")], vec![2], vec![2]),
                block_p(2, vec![instr("Jump")], vec![1], vec![1]),
            ],
        );
        let (p, id) = one_func_program(f);
        let enc = encode_func(&p, id).unwrap();
        let text = enc.reach_query(1, vec![]).canonical_text();
        assert!(
            text.contains("(assert (= g1 false))"),
            "unreachable block 1 must be pinned false:\n{text}"
        );
        assert!(
            text.contains("(assert (= g2 false))"),
            "unreachable block 2 must be pinned false:\n{text}"
        );
        // The no-FP guarantee: reaching an unreachable block is Unsat.
        let mut solver = goverify_solver::Z3Native::new(goverify_solver::SolverLimits {
            timeout_ms: 5_000,
            ..Default::default()
        });
        let outcome =
            goverify_solver::discharge_query(&enc.reach_query(1, vec![]), &mut solver, None, None);
        assert_eq!(
            outcome.result,
            goverify_solver::SatResult::Unsat,
            "an unreachable block can never be reached: {outcome:?}"
        );
    }

    #[test]
    fn straight_line_guards_entry_true_then_propagate() {
        // 0 -Jump-> 1. g0 = true, g1 = g0.
        let f = func(
            "t.F",
            vec![
                block_p(0, vec![instr("Jump")], vec![], vec![1]),
                block_p(1, vec![instr("Return")], vec![0], vec![]),
            ],
        );
        let (p, id) = one_func_program(f);
        let enc = encode_func(&p, id).unwrap();
        assert_eq!(enc.guards.len(), 2);
        // Entry guard asserted true; g1 defined from g0's edge. Pin via
        // the canonical query text — the stable, printable artifact.
        let q = enc.reach_query(1, vec![]);
        let text = q.canonical_text();
        assert!(
            text.contains("(assert (= g0 true))"),
            "entry guard:\n{text}"
        );
        assert!(text.contains("(assert (= g1 g0))"), "jump edge:\n{text}");
        assert!(text.contains("(assert g1)"), "reach conjunct:\n{text}");
    }

    #[test]
    fn branch_guards_split_on_condition() {
        // Block 0 ends in Branch(cond v1: bool param p0); succs [1, 2].
        // g1 = (and g0 p0), g2 = (and g0 (not p0)).
        let mut br = instr("If");
        br.operands = vec![1];
        let f = {
            let mut f = func(
                "t.F",
                vec![
                    block_p(0, vec![br], vec![], vec![1, 2]),
                    block_p(1, vec![instr("Return")], vec![0], vec![]),
                    block_p(2, vec![instr("Return")], vec![0], vec![]),
                ],
            );
            f.params = vec![goverify_extract::gvir::Param {
                id: 1,
                name: "c".into(),
                r#type: 3, // bool in the fixture type table
            }];
            f
        };
        // (fixture needs the bool type interned — reuse types_program's
        // type list in `pkg`'s package or add types to the builder)
        let (p, id) = one_func_program_with_bool_type(f);
        let enc = encode_func(&p, id).unwrap();
        let text = enc.reach_query(2, vec![]).canonical_text();
        assert!(text.contains("(= g1 (and g0 p0))"), "then edge:\n{text}");
        assert!(
            text.contains("(= g2 (and g0 (not p0)))"),
            "else edge:\n{text}"
        );
    }

    /// `one_func_program` interns no types at all (the straight-line test
    /// above never reads a param's type), so the branch test needs its own
    /// builder that interns a `bool` type at local id 3 (matching the
    /// param's `r#type: 3` above) before lowering.
    fn one_func_program_with_bool_type(f: goverify_extract::gvir::Function) -> (Program, FuncId) {
        // `TypeTable::import_package` caps its local-id map at
        // `types.len()` (any id beyond that degrades to Unknown), so the
        // package needs dense ids 1..=3 with "bool" landing on id 3 to
        // match the param's `r#type: 3` above.
        use goverify_extract::gvir;
        let package = gvir::Package {
            import_path: "t".into(),
            types: vec![
                gvir::Type {
                    id: 1,
                    repr: "int".into(),
                    kind: gvir::TypeKind::Basic as i32,
                    name: "int".into(),
                    ..Default::default()
                },
                gvir::Type {
                    id: 2,
                    repr: "uint8".into(),
                    kind: gvir::TypeKind::Basic as i32,
                    name: "uint8".into(),
                    ..Default::default()
                },
                gvir::Type {
                    id: 3,
                    repr: "bool".into(),
                    kind: gvir::TypeKind::Basic as i32,
                    name: "bool".into(),
                    ..Default::default()
                },
            ],
            functions: vec![f],
            ..Default::default()
        };
        let p = Program::from_packages(vec![package]);
        let id = p.lookup_func("t.F").unwrap();
        (p, id)
    }

    #[test]
    fn merge_block_guard_is_or_of_incoming() {
        // diamond: 0 -> {1,2} -> 3. g3 = (or <edge from 1> <edge from 2>).
        // Edge guards from Jump blocks pass through: g3 = (or g1 g2).
        let f = func(
            "t.F",
            vec![
                block_p(0, vec![instr("Jump")], vec![], vec![1, 2]),
                block_p(1, vec![instr("Jump")], vec![0], vec![3]),
                block_p(2, vec![instr("Jump")], vec![0], vec![3]),
                block_p(3, vec![instr("Return")], vec![1, 2], vec![]),
            ],
        );
        let (p, id) = one_func_program(f);
        let enc = encode_func(&p, id).unwrap();
        let text = enc.reach_query(3, vec![]).canonical_text();
        assert!(text.contains("(= g3 (or g1 g2))"), "merge guard:\n{text}");
    }

    // ---- Task 4: op semantics & phi ----------------------------------

    /// Dense type table (ids 1..=6) shared by the op-encoding fixtures:
    /// 1 int, 2 uint8, 3 bool, 4 struct T, 5 *T, 6 []int.
    fn std_type_list() -> Vec<goverify_extract::gvir::Type> {
        use goverify_extract::gvir;
        let basic = |id: u32, repr: &str| gvir::Type {
            id,
            repr: repr.into(),
            kind: gvir::TypeKind::Basic as i32,
            name: repr.into(),
            ..Default::default()
        };
        vec![
            basic(1, "int"),
            basic(2, "uint8"),
            basic(3, "bool"),
            gvir::Type {
                id: 4,
                repr: "T".into(),
                kind: gvir::TypeKind::Struct as i32,
                ..Default::default()
            },
            gvir::Type {
                id: 5,
                repr: "*T".into(),
                kind: gvir::TypeKind::Pointer as i32,
                elem: 4,
                ..Default::default()
            },
            gvir::Type {
                id: 6,
                repr: "[]int".into(),
                kind: gvir::TypeKind::Slice as i32,
                elem: 1,
                ..Default::default()
            },
            basic(7, "uintptr"),
            gvir::Type {
                id: 8,
                repr: "unsafe.Pointer".into(),
                kind: gvir::TypeKind::Basic as i32,
                name: "Pointer".into(),
                ..Default::default()
            },
        ]
    }

    fn program_with(f: goverify_extract::gvir::Function) -> (Program, FuncId) {
        use goverify_extract::gvir;
        let package = gvir::Package {
            import_path: "t".into(),
            types: std_type_list(),
            functions: vec![f],
            ..Default::default()
        };
        let p = Program::from_packages(vec![package]);
        let id = p.lookup_func("t.F").unwrap();
        (p, id)
    }

    fn param(id: u32, ty: u32) -> goverify_extract::gvir::Param {
        goverify_extract::gvir::Param {
            id,
            name: format!("a{id}"),
            r#type: ty,
        }
    }

    fn binop(reg: u32, ty: u32, op: &str, l: u32, r: u32) -> goverify_extract::gvir::Instruction {
        use goverify_extract::gvir;
        gvir::Instruction {
            kind: "BinOp".into(),
            register: reg,
            r#type: ty,
            operands: vec![l, r],
            sem: Some(gvir::instruction::Sem::Binop(gvir::BinOpSem {
                op: op.into(),
            })),
            ..Default::default()
        }
    }

    fn ret(vals: Vec<u32>) -> goverify_extract::gvir::Instruction {
        goverify_extract::gvir::Instruction {
            kind: "Return".into(),
            operands: vals,
            ..Default::default()
        }
    }

    #[test]
    fn binop_add_encodes_bvadd() {
        use goverify_extract::gvir;
        // v2 = p0 + 1 (int): p0 = param id 1, the const 1 = aux id 3.
        // Operands mirror lower.rs's BinOp arm: [lhs, rhs] = [1, 3].
        let f = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 1)],
            aux: vec![gvir::AuxValue {
                id: 3,
                kind: "Const".into(),
                r#type: 1,
                r#const: Some(gvir::ConstValue {
                    value: Some(gvir::const_value::Value::Int(1)),
                }),
                ..Default::default()
            }],
            blocks: vec![block_p(
                0,
                vec![binop(2, 1, "+", 1, 3), ret(vec![2])],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        // Operands are declared vars; the const's value lands in its own
        // defining equality (printer bv-literal syntax: `(_ bv1 64)`).
        assert!(text.contains("(= v2 (bvadd p0 v3))"), "add def:\n{text}");
        assert!(
            text.contains("(= v3 (_ bv1 64))"),
            "const 1 literal:\n{text}"
        );
    }

    #[test]
    fn address_of_ops_assert_nonnil_dst() {
        use goverify_extract::gvir;
        // f(p *T): v2 = FieldAddr p0 .0 — fix-wave fix 1: the dst of an
        // address-of op is never nil; the encoding must carry that fact.
        let f = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 5)],
            blocks: vec![block_p(
                0,
                vec![
                    gvir::Instruction {
                        kind: "FieldAddr".into(),
                        register: 2,
                        r#type: 5,
                        operands: vec![1],
                        sem: Some(gvir::instruction::Sem::Field(gvir::FieldSem {
                            index: 0,
                            name: "X".into(),
                        })),
                        ..Default::default()
                    },
                    ret(vec![2]),
                ],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let enc = encode_func(&p, id).unwrap();
        let d = enc.value(goverify_ir::ValueId(2)).unwrap().clone();
        let want = Term::not(goverify_solver::ptr_is_nil(d).unwrap()).unwrap();
        assert!(
            enc.asserts.contains(&want),
            "FieldAddr dst must carry a non-nil assert (fix 1)"
        );
    }

    /// Shared builder for the fix-3 uintptr-provenance tests: `reg =
    /// Convert src`, with an explicit dst type (`*T` id 5 or
    /// unsafe.Pointer id 8).
    fn convert_ins(reg: u32, ty: u32, src: u32) -> goverify_extract::gvir::Instruction {
        use goverify_extract::gvir;
        gvir::Instruction {
            kind: "Convert".into(),
            register: reg,
            r#type: ty,
            operands: vec![src],
            ..Default::default()
        }
    }

    #[test]
    fn uintptr_convert_chain_asserts_nonnil() {
        // f(p0 uintptr): v2 = Convert p0 -> unsafe.Pointer;
        //                v3 = Convert v2 -> *T.
        // The canonical (*T)(unsafe.Pointer(uintptr...)) idiom: v3 must
        // carry a non-nil assert (fix 3).
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 7)], // uintptr
            blocks: vec![block_p(
                0,
                vec![
                    convert_ins(2, 8, 1), // v2 = Convert p0(uintptr) -> Pointer
                    convert_ins(3, 5, 2), // v3 = Convert v2(Pointer) -> *T
                    ret(vec![3]),
                ],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let enc = encode_func(&p, id).unwrap();
        let d = enc.value(goverify_ir::ValueId(3)).unwrap().clone();
        let want = Term::not(goverify_solver::ptr_is_nil(d).unwrap()).unwrap();
        assert!(
            enc.asserts.contains(&want),
            "uintptr-derived pointer non-nil (fix 3)"
        );
    }

    #[test]
    fn pointer_pun_stays_nilable() {
        // f(p0 *T): v2 = Convert p0 -> unsafe.Pointer; v3 = Convert v2 -> *T.
        // A plain pointer pun preserves nilability: NO non-nil assert on v3.
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 5)], // *T
            blocks: vec![block_p(
                0,
                vec![
                    convert_ins(2, 8, 1), // v2 = Convert p0(*T) -> Pointer
                    convert_ins(3, 5, 2), // v3 = Convert v2(Pointer) -> *T
                    ret(vec![3]),
                ],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let enc = encode_func(&p, id).unwrap();
        let d = enc.value(goverify_ir::ValueId(3)).unwrap().clone();
        let unwanted = Term::not(goverify_solver::ptr_is_nil(d).unwrap()).unwrap();
        assert!(
            !enc.asserts.contains(&unwanted),
            "pointer pun must stay nilable (fix 3 red)"
        );
    }

    #[test]
    fn fanout_convert_cycle_on_crafted_gvir_terminates() {
        // Crafted, NOT legitimate SSA (single-def-per-register is a
        // go/ssa invariant, unenforced on untrusted `.gvir` bytes): 12
        // `Convert` instructions all write the SAME dst v5
        // (unsafe.Pointer-typed) from itself, followed by a Ptr-sorted
        // Convert consuming v5. Pre-fix, `uintptr_provenance`'s depth cap
        // bounded PATH length only — every level's `.any()` matched all
        // 12 self-referencing instructions and re-recursed into all of
        // them, so total work grew ~12^depth. This test's value is that
        // it completes AT ALL (review fix: `uintptr_provenance` now
        // memoizes visited ValueIds, so revisiting v5 short-circuits to
        // "no" after the first visit — total work bounded, not just
        // per-path depth).
        const FANOUT: u32 = 12;
        let mut instrs: Vec<goverify_extract::gvir::Instruction> =
            (0..FANOUT).map(|_| convert_ins(5, 8, 5)).collect();
        instrs.push(convert_ins(6, 5, 5)); // v6 = Convert v5(Pointer) -> *T
        instrs.push(ret(vec![6]));
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            blocks: vec![block_p(0, instrs, vec![], vec![])],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let enc = encode_func(&p, id).unwrap();
        let d = enc.value(goverify_ir::ValueId(6)).unwrap().clone();
        let unwanted = Term::not(goverify_solver::ptr_is_nil(d).unwrap()).unwrap();
        assert!(
            !enc.asserts.contains(&unwanted),
            "crafted fan-out/self-loop cycle must degrade to no provenance, not hang (review fix)"
        );
    }

    /// Shared builder for the fix-2a load-forwarding tests: a FieldAddr at
    /// `.0` off `base`, field/dst type `*T` (id 5) so the loaded value is
    /// Ptr-sorted (modelable — the forwarding assert needs a real term on
    /// both dsts).
    fn field_addr_ins(reg: u32, base: u32) -> goverify_extract::gvir::Instruction {
        use goverify_extract::gvir;
        gvir::Instruction {
            kind: "FieldAddr".into(),
            register: reg,
            r#type: 5, // *T
            operands: vec![base],
            sem: Some(gvir::instruction::Sem::Field(gvir::FieldSem {
                index: 0,
                name: "X".into(),
            })),
            ..Default::default()
        }
    }

    /// `*addr` (go/ssa UnOp "*"): the only lowering that produces `Op::Load`.
    fn load_ins(reg: u32, addr: u32) -> goverify_extract::gvir::Instruction {
        use goverify_extract::gvir;
        gvir::Instruction {
            kind: "UnOp".into(),
            register: reg,
            r#type: 5, // *T
            operands: vec![addr],
            sem: Some(gvir::instruction::Sem::Unop(gvir::UnOpSem {
                op: "*".into(),
                comma_ok: false,
            })),
            ..Default::default()
        }
    }

    #[test]
    fn repeated_field_load_is_forwarded() {
        // f(p0 *T): v2=FieldAddr p0 .0; v3=Load v2; v4=FieldAddr p0 .0; v5=Load v4.
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 5)],
            blocks: vec![block_p(
                0,
                vec![
                    field_addr_ins(2, 1),
                    load_ins(3, 2),
                    field_addr_ins(4, 1),
                    load_ins(5, 4),
                    ret(vec![5]),
                ],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let enc = encode_func(&p, id).unwrap();
        let d = enc.value(goverify_ir::ValueId(5)).unwrap().clone();
        let prev = enc.value(goverify_ir::ValueId(3)).unwrap().clone();
        let want = Term::eq(d, prev).unwrap();
        assert!(
            enc.asserts.contains(&want),
            "second load forwarded to first (fix 2a)"
        );
    }

    #[test]
    fn store_between_loads_kills_forwarding() {
        // Same shape, with `Store v2 <- p0` inserted between the two loads.
        let store_ins = goverify_extract::gvir::Instruction {
            kind: "Store".into(),
            operands: vec![2, 1], // addr = v2, val = p0
            ..Default::default()
        };
        let f = goverify_extract::gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 5)],
            blocks: vec![block_p(
                0,
                vec![
                    field_addr_ins(2, 1),
                    load_ins(3, 2),
                    store_ins,
                    field_addr_ins(4, 1),
                    load_ins(5, 4),
                    ret(vec![5]),
                ],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let enc = encode_func(&p, id).unwrap();
        let d = enc.value(goverify_ir::ValueId(5)).unwrap().clone();
        let prev = enc.value(goverify_ir::ValueId(3)).unwrap().clone();
        let eq = Term::eq(d, prev).unwrap();
        assert!(
            !enc.asserts.contains(&eq),
            "a Store must invalidate forwarding"
        );
    }

    #[test]
    fn signed_and_unsigned_div_pick_sdiv_udiv() {
        use goverify_extract::gvir;
        // `a / b`: int operands -> bvsdiv; uint8 operands -> bvudiv.
        let mk = |ty: u32| gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, ty), param(2, ty)],
            blocks: vec![block_p(
                0,
                vec![binop(3, ty, "/", 1, 2), ret(vec![3])],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(mk(1));
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(
            text.contains("(= v3 (bvsdiv p0 p1))"),
            "signed div:\n{text}"
        );
        let (p, id) = program_with(mk(2));
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(
            text.contains("(= v3 (bvudiv p0 p1))"),
            "unsigned div:\n{text}"
        );
    }

    #[test]
    fn comparison_binops_encode_bool_dsts() {
        use goverify_extract::gvir;
        // `a < b`: signedness from the OPERAND type. int -> bvslt,
        // uint8 -> bvult; the dst is Bool either way.
        let mk = |ty: u32| gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, ty), param(2, ty)],
            blocks: vec![block_p(
                0,
                vec![binop(3, 3, "<", 1, 2), ret(vec![3])],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(mk(1));
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(text.contains("(= v3 (bvslt p0 p1))"), "signed lt:\n{text}");
        let (p, id) = program_with(mk(2));
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(
            text.contains("(= v3 (bvult p0 p1))"),
            "unsigned lt:\n{text}"
        );
    }

    #[test]
    fn phi_is_ite_over_incoming_edge_guards() {
        use goverify_extract::gvir;
        // diamond 0 -> {1,2} -> 3; phi at 3 merges p0,p1 over edges from
        // blocks 1,2 (both Jump). preds [1,2], edges [p0,p1]. The 1->3
        // edge guard is g1 (Jump passes the guard through); with two
        // incoming edges the last edge is the ite's else branch.
        let br = gvir::Instruction {
            kind: "If".into(),
            operands: vec![3], // cond = p2 (bool)
            ..Default::default()
        };
        let phi = gvir::Instruction {
            kind: "Phi".into(),
            register: 4,
            r#type: 1,
            operands: vec![1, 2],
            ..Default::default()
        };
        let f = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 1), param(2, 1), param(3, 3)],
            blocks: vec![
                block_p(0, vec![br], vec![], vec![1, 2]),
                block_p(1, vec![instr("Jump")], vec![0], vec![3]),
                block_p(2, vec![instr("Jump")], vec![0], vec![3]),
                block_p(3, vec![phi, ret(vec![4])], vec![1, 2], vec![]),
            ],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(3, vec![])
            .canonical_text();
        assert!(text.contains("(= v4 (ite g1 p0 p1))"), "phi ite:\n{text}");
    }

    #[test]
    fn phi_with_cut_back_edge_keeps_preheader_value_only() {
        use goverify_extract::gvir;
        // loop: 0 -> 1(header); 1 -> {2,3}; 2 -> 1 (back edge cut); 3 exit.
        // phi at header, preds [0,2], edges [p0(preheader), p1(loop)].
        // The 2->1 edge is cut, so the phi collapses to just p0 — no ite.
        let br = gvir::Instruction {
            kind: "If".into(),
            operands: vec![3],
            ..Default::default()
        };
        let phi = gvir::Instruction {
            kind: "Phi".into(),
            register: 4,
            r#type: 1,
            operands: vec![1, 2],
            ..Default::default()
        };
        let f = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 1), param(2, 1), param(3, 3)],
            blocks: vec![
                block_p(0, vec![instr("Jump")], vec![], vec![1]),
                block_p(1, vec![phi, br], vec![0, 2], vec![2, 3]),
                block_p(2, vec![instr("Jump")], vec![1], vec![1]),
                block_p(3, vec![ret(vec![4])], vec![1], vec![]),
            ],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(1, vec![])
            .canonical_text();
        assert!(text.contains("(= v4 p0)"), "phi keeps preheader:\n{text}");
        assert!(!text.contains("ite"), "no ite for cut back edge:\n{text}");
    }

    #[test]
    fn makeslice_defines_len_and_cap() {
        use goverify_extract::gvir;
        // Make{Slice, args [len, cap]} => (= v3 (seq-val p0 p1)).
        let mk = gvir::Instruction {
            kind: "MakeSlice".into(),
            register: 3,
            r#type: 6, // []int
            operands: vec![1, 2],
            ..Default::default()
        };
        let f = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 1), param(2, 1)],
            blocks: vec![block_p(0, vec![mk, ret(vec![3])], vec![], vec![])],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(
            text.contains("(= v3 (seq-val p0 p1))"),
            "makeslice len/cap:\n{text}"
        );
    }

    #[test]
    fn len_builtin_defines_seq_len_accessor() {
        use goverify_extract::gvir;
        // v2 = len(s), s: []int param — Task 8's addition: the "len"
        // builtin call ties its dst to the base's own seq-len accessor
        // instead of havoc'ing, so a `len(s)`-guarded bounds check can
        // actually be proven safe downstream (goverify-checkers'
        // BoundsChecker consumes this).
        let lencall = gvir::Instruction {
            kind: "Call".into(),
            register: 2,
            r#type: 1,            // int
            operands: vec![0, 1], // [callee slot (unused), s]
            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                builtin: "len".into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let f = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 6)], // s: []int
            blocks: vec![block_p(0, vec![lencall, ret(vec![2])], vec![], vec![])],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(
            text.contains("(= v2 (seq-len p0))"),
            "len builtin ties to seq-len:\n{text}"
        );
    }

    #[test]
    fn len_builtin_on_non_seq_arg_havocs() {
        use goverify_extract::gvir;
        // len() on a non-Seq-sorted arg (e.g. an unmodelable/mismatched
        // type) must degrade to havoc, never error or panic.
        let lencall = gvir::Instruction {
            kind: "Call".into(),
            register: 2,
            r#type: 1, // int
            operands: vec![0, 1],
            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                builtin: "len".into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let f = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 1)], // int, not a Seq
            blocks: vec![block_p(0, vec![lencall, ret(vec![2])], vec![], vec![])],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let enc = encode_func(&p, id).expect("must not error on a mismatched len() arg");
        let text = enc.reach_query(0, vec![]).canonical_text();
        assert!(
            text.contains("(declare-const v2 (_ BitVec 64))"),
            "len() dst still declared:\n{text}"
        );
        assert!(
            !text.contains("(= v2"),
            "non-Seq arg: len() dst has no defining equality (havoc'd):\n{text}"
        );
    }

    #[test]
    fn make_slice_dst_keeps_seq_invariant_but_slice_dst_does_not() {
        use goverify_extract::gvir;
        // Review finding (Task 8 fast-follow): the two Seq-producing ops
        // are NOT symmetric. `Make{Slice}`'s len<=cap invariant is
        // `make`'s own runtime precondition (Go panics on m>n; no
        // reachable execution has m>n past it) and must stay asserted.
        // `Op::Slice`'s new len/cap are a raw `high-low`/`cap-low`
        // derivation that can legitimately disagree with len<=cap on an
        // out-of-bounds slice (that disagreement IS the bug), so it must
        // NOT be asserted there.
        let mk = gvir::Instruction {
            kind: "MakeSlice".into(),
            register: 3,
            r#type: 6, // []int
            operands: vec![1, 2],
            ..Default::default()
        };
        let f = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 1), param(2, 1)],
            blocks: vec![block_p(0, vec![mk, ret(vec![3])], vec![], vec![])],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(
            text.contains("(bvule (seq-len v3) (seq-cap v3))"),
            "Make{{Slice}} dst DOES carry the seq invariant:\n{text}"
        );

        let sl = gvir::Instruction {
            kind: "Slice".into(),
            register: 3,
            r#type: 6,         // []int
            operands: vec![1], // base = p0 ([]int param), low/high/max absent
            ..Default::default()
        };
        let f2 = gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 6)], // []int
            blocks: vec![block_p(0, vec![sl, ret(vec![3])], vec![], vec![])],
            ..Default::default()
        };
        let (p2, id2) = program_with(f2);
        let text2 = encode_func(&p2, id2)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(
            !text2.contains("(bvule (seq-len v3) (seq-cap v3))"),
            "Slice dst does NOT carry the seq invariant:\n{text2}"
        );
    }

    #[test]
    fn unmodeled_ops_havoc_but_declare() {
        use goverify_extract::gvir;
        // A Call dst of type *T gets a Ptr-sorted const with NO defining
        // equality; encode_func must not error.
        let callins = gvir::Instruction {
            kind: "Call".into(),
            register: 2,
            r#type: 5, // *T
            operands: vec![0],
            sem: Some(gvir::instruction::Sem::Call(gvir::CallSem {
                static_callee: "t.G".into(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let f = gvir::Function {
            id: "t.F".into(),
            blocks: vec![block_p(0, vec![callins, ret(vec![2])], vec![], vec![])],
            ..Default::default()
        };
        let (p, id) = program_with(f);
        let enc = encode_func(&p, id).expect("unmodeled op must not error");
        let text = enc.reach_query(0, vec![]).canonical_text();
        assert!(
            text.contains("(declare-const v2 Ptr)"),
            "call dst declared:\n{text}"
        );
        assert!(
            !text.contains("(= v2"),
            "call dst has no defining equality:\n{text}"
        );
    }

    #[test]
    fn eq_neq_work_across_sorts() {
        use goverify_extract::gvir;
        // BinOp Eq on pointers => (= v3 (= p0 p1)); Neq => (not (= …)).
        let mk = |op: &str| gvir::Function {
            id: "t.F".into(),
            params: vec![param(1, 5), param(2, 5)], // *T pointers
            blocks: vec![block_p(
                0,
                vec![binop(3, 3, op, 1, 2), ret(vec![3])],
                vec![],
                vec![],
            )],
            ..Default::default()
        };
        let (p, id) = program_with(mk("=="));
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(text.contains("(= v3 (= p0 p1))"), "ptr eq:\n{text}");
        let (p, id) = program_with(mk("!="));
        let text = encode_func(&p, id)
            .unwrap()
            .reach_query(0, vec![])
            .canonical_text();
        assert!(text.contains("(= v3 (not (= p0 p1)))"), "ptr neq:\n{text}");
    }

    // ---- Task 10: trace reconstruction from sat models ---------------

    #[test]
    fn guard_values_parses_z3native_arrow_format() {
        let m = "g0 -> true\ng1 -> false\np0 -> (ptr-addr #x0000000000000001)\n";
        let g = guard_values(m);
        assert_eq!(g.get("g0"), Some(&true));
        assert_eq!(g.get("g1"), Some(&false));
        assert_eq!(g.get("p0"), None, "non-bool ignored");
    }

    #[test]
    fn guard_values_parses_define_fun_format() {
        let m = "((define-fun g0 () Bool true)\n (define-fun g2 () Bool false))";
        let g = guard_values(m);
        assert_eq!(g.get("g0"), Some(&true));
        assert_eq!(g.get("g2"), Some(&false));
    }

    #[test]
    fn guard_values_tolerates_garbage() {
        assert!(guard_values("((((").is_empty());
        assert!(guard_values("").is_empty());
    }

    #[test]
    fn violating_path_walks_true_guards() {
        // diamond 0->{1,2}->3; guards g0,g1,g3 true, g2 false:
        // path = [0, 1, 3].
        let f = func(
            "t.F",
            vec![
                block_p(0, vec![instr("Jump")], vec![], vec![1, 2]),
                block_p(1, vec![instr("Jump")], vec![0], vec![3]),
                block_p(2, vec![instr("Jump")], vec![0], vec![3]),
                block_p(3, vec![instr("Return")], vec![1, 2], vec![]),
            ],
        );
        let (p, id) = one_func_program(f);
        let func_ref = p.func(id).unwrap();
        let dag = cut_back_edges(func_ref);
        let guards: BTreeMap<String, bool> = [
            ("g0".to_string(), true),
            ("g1".to_string(), true),
            ("g2".to_string(), false),
            ("g3".to_string(), true),
        ]
        .into_iter()
        .collect();
        let path = violating_path(func_ref, &dag, &guards);
        assert_eq!(path, vec![0, 1, 3]);
    }

    #[test]
    fn violating_path_stops_at_dont_care() {
        // g0 true, successors' guards unassigned: path = [0].
        let f = func(
            "t.F",
            vec![
                block_p(0, vec![instr("Jump")], vec![], vec![1, 2]),
                block_p(1, vec![instr("Jump")], vec![0], vec![3]),
                block_p(2, vec![instr("Jump")], vec![0], vec![3]),
                block_p(3, vec![instr("Return")], vec![1, 2], vec![]),
            ],
        );
        let (p, id) = one_func_program(f);
        let func_ref = p.func(id).unwrap();
        let dag = cut_back_edges(func_ref);
        let guards: BTreeMap<String, bool> = [("g0".to_string(), true)].into_iter().collect();
        let path = violating_path(func_ref, &dag, &guards);
        assert_eq!(path, vec![0]);
    }

    #[test]
    fn model_bindings_extracts_display_strings() {
        let m = "p0 -> (ptr-nil)\nv3 -> #x0000000000000005\n";
        let b = model_bindings(m);
        assert_eq!(b.get("p0").map(String::as_str), Some("(ptr-nil)"));
    }

    #[test]
    fn encoding_is_deterministic() {
        let p = goverify_ir::testutil::load_corpus("ops");
        for f in p.func_ids() {
            if p.func(f).is_none() {
                continue;
            }
            let (Ok(a), Ok(b)) = (encode_func(&p, f), encode_func(&p, f)) else {
                continue;
            };
            for bi in 0..a.guards.len() {
                assert_eq!(
                    a.reach_query(bi, vec![]).canonical_text(),
                    b.reach_query(bi, vec![]).canonical_text(),
                    "{} block {bi}",
                    p.func_name(f)
                );
            }
        }
    }
}
