//! Gated-SSA function encoding (phase-4 spec §3): one whole-function
//! SMT encoding — block guards, phi-as-ite, back edges cut (loops are
//! analyzed as their first iteration; reachability stays
//! under-approximate, so Sat always witnesses a genuine execution).
//! Pure: `Program` in, declarations + assertions out. Anything not
//! modeled havocs — degrade, never die.

use std::collections::BTreeMap;

use goverify_ir::{
    ConstVal, FuncId, Function, Op, Program, TypeId, TypeKind, TypeTable, ValueId, ValueKind,
};
use goverify_solver::{
    BvCmpOp, CtorDecl, DatatypeDecl, Logic, Query, Sort, Term, ptr_datatype, ptr_nil, ptr_sort,
};

/// Slices/strings as length-carrying opaque values: contents havoc,
/// bounds reasoning uses len/cap only. Invariant 0 <= len <= cap is
/// asserted for every Seq-sorted value the encoder introduces.
pub fn seq_datatype() -> DatatypeDecl {
    DatatypeDecl {
        name: "Seq".into(),
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
/// blocks keep their edges (harmless: their guards become false).
pub fn cut_back_edges(f: &Function) -> Vec<Vec<u32>> {
    let n = f.blocks.len();
    let mut dag: Vec<Vec<u32>> = f.blocks.iter().map(|b| b.succs.clone()).collect();
    let mut state = vec![0u8; n]; // 0 unvisited, 1 on stack, 2 done
    // Iterative DFS; (block, next-succ-index) frames.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    if n == 0 {
        return dag;
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
    dag
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
    let dag_succs = cut_back_edges(func);
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
    // Params first: their terms exist before any op reads them.
    for &pv in &func.params {
        declare_value(p, func, pv, &mut enc);
    }
    // Pass 1: declare/const-define every value an instruction reads or
    // writes (Task 4 fills op semantics; this pass makes havoc sound).
    // Pass 2 (below): guard structure.
    encode_guards(func, &mut enc)?;
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
    if sort == seq_datatype().sort() {
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
fn encode_guards(func: &Function, enc: &mut EncodedFunc) -> Result<(), String> {
    let n = func.blocks.len();
    // edge_guard[(from, to)] considering only DAG edges
    let mut incoming: Vec<Vec<Term>> = vec![Vec::new(); n];
    for (b, block) in func.blocks.iter().enumerate() {
        let gb = enc.guards[b].clone();
        let cond = block.instrs.last().and_then(|i| match &i.op {
            Op::Branch { cond } => enc.values.get(cond).cloned().or_else(|| {
                // Unmodelable condition: havoc it as a fresh bool so both
                // branches stay possible (missing info = nondeterminism).
                let name = format!("hc{b}");
                enc.consts.push((name.clone(), Sort::Bool));
                Some(Term::var(&name, Sort::Bool))
            }),
            _ => None,
        });
        for &s in &enc.dag_succs[b] {
            let s = s as usize;
            if s >= n {
                continue;
            }
            // Positional index within the ORIGINAL succs decides the
            // branch polarity (dag_succs preserves order).
            let orig_pos = func.blocks[b].succs.iter().position(|&x| x as usize == s);
            let edge = match (&cond, orig_pos) {
                (Some(c), Some(0)) => Term::and(vec![gb.clone(), c.clone()]),
                (Some(c), Some(1)) => {
                    Term::not(c.clone()).and_then(|nc| Term::and(vec![gb.clone(), nc]))
                }
                _ => Ok(gb.clone()),
            }
            .map_err(|e| format!("edge guard: {e}"))?;
            incoming[s].push(edge);
        }
    }
    for (b, edges) in incoming.into_iter().enumerate() {
        let gb = enc.guards[b].clone();
        let rhs = if b == 0 {
            Term::bool_lit(true)
        } else if edges.is_empty() {
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
}
