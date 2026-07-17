//! Canonical text renderings of the IR (phase-2 spec §7). These strings
//! are a determinism surface: byte-compared across runs in CI. Iterate
//! vectors only — a HashMap iteration here is a bug.
//!
//! Format (fixed): one function per stanza.
//!   func <name> (v<N> <type>, ...)
//!     aux v<N> = const <val>          -- non-instr values, sorted by id
//!     b<N> -> [succ, ...]
//!       v<N> = <op> ...               -- one instruction per line, 4-space indent
//!
//! Values are always printed as `v<N>`; types are printed in parens only
//! for params. Struct field ops (`field-addr`/`field`) print the resolved
//! field name after `#<index>` when the base's type resolves to a struct
//! (peeling `Named`/`Pointer`), e.g. `field-addr v1 #0 mu`. Calls print
//! the callee: `call t.G(v2)` (static), `call-invoke io.Closer.Close(v1)`
//! (interface method), `call-builtin len(v1)` (builtin), `call-dyn
//! v4(v2)` (function value). Everything here iterates vectors in stored
//! order, never a map — required for byte-identical output across runs.

use std::fmt::Write;

use crate::func::{Function, ValueId, ValueKind};
use crate::op::{Callee, Op};
use crate::program::{FuncId, Program};
use crate::types::TypeKind;

/// Render one function's lowered body as canonical text. Bodyless
/// (external) functions print a one-line stub. Never panics: this reads
/// lowered IR that was itself built to be total over malformed/fuzzed
/// input (`Op::Havoc`, `ValueKind::Opaque`), so dumping it must stay total
/// too.
pub fn dump_function(p: &Program, id: FuncId) -> String {
    let Some(f) = p.func(id) else {
        return format!("func {} <external>\n", p.func_name(id));
    };
    let mut s = String::new();
    let params = f
        .params
        .iter()
        .map(|&v| format!("v{} {}", v.0, p.types().repr(f.value(v).ty)))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(s, "func {} ({params})", p.func_name(id));
    for (i, info) in f.values.iter().enumerate() {
        match &info.kind {
            ValueKind::Const(c) => {
                let _ = writeln!(s, "  aux v{i} = const {c:?}");
            }
            ValueKind::Global(g) => {
                let _ = writeln!(s, "  aux v{i} = global {g}");
            }
            ValueKind::FuncRef(fid) => {
                let _ = writeln!(s, "  aux v{i} = func {}", p.func_name(*fid));
            }
            ValueKind::Builtin(b) => {
                let _ = writeln!(s, "  aux v{i} = builtin {b}");
            }
            _ => {}
        }
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        let _ = writeln!(s, "  b{bi} -> {:?}", b.succs);
        for ins in &b.instrs {
            let _ = writeln!(s, "    {}", render_op(p, f, &ins.op));
        }
    }
    s
}

fn render_callee(p: &Program, c: &Callee) -> String {
    match c {
        Callee::Static(f) => format!("call {}", p.func_name(*f)),
        Callee::Invoke { iface, method, .. } => {
            format!("call-invoke {}.{method}", p.types().repr(*iface))
        }
        Callee::Builtin(b) => format!("call-builtin {b}"),
        Callee::Dynamic { value } => format!("call-dyn v{}", value.0),
    }
}

fn vlist(vs: &[ValueId]) -> String {
    vs.iter()
        .map(|v| format!("v{}", v.0))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve the field name at `field` on `base`'s type, peeling `Named`
/// and `Pointer` wrappers to reach a `Struct`. Returns `None` (no suffix
/// printed) whenever the type doesn't resolve to a struct, or the index
/// is out of range — this reads types built from possibly-malformed
/// `.gvir` input, so it must degrade rather than panic or loop forever.
fn resolve_field_name<'a>(
    p: &'a Program,
    f: &Function,
    base: ValueId,
    field: u32,
) -> Option<&'a str> {
    let mut ty = f.value(base).ty;
    // Bound the peel: legitimate type graphs are shallow (a handful of
    // Named/Pointer layers at most); this also stops a cyclic/malformed
    // graph from looping.
    for _ in 0..16 {
        match p.types().kind(ty) {
            TypeKind::Named { underlying, .. } => ty = *underlying,
            TypeKind::Pointer { elem } => ty = *elem,
            TypeKind::Struct { fields } => {
                return fields.get(field as usize).map(|fi| fi.name.as_str());
            }
            _ => return None,
        }
    }
    None
}

fn render_op(p: &Program, f: &Function, op: &Op) -> String {
    match op {
        Op::Assign { dst, src } => format!("v{} = assign v{}", dst.0, src.0),
        Op::Alloc { dst, heap } => format!("v{} = alloc heap={heap}", dst.0),
        Op::Load { dst, addr } => format!("v{} = load v{}", dst.0, addr.0),
        Op::Store { addr, val } => format!("store v{} <- v{}", addr.0, val.0),
        Op::FieldAddr { dst, base, field } => {
            let suffix = resolve_field_name(p, f, *base, *field)
                .map(|n| format!(" {n}"))
                .unwrap_or_default();
            format!("v{} = field-addr v{} #{field}{suffix}", dst.0, base.0)
        }
        Op::Field { dst, base, field } => {
            let suffix = resolve_field_name(p, f, *base, *field)
                .map(|n| format!(" {n}"))
                .unwrap_or_default();
            format!("v{} = field v{} #{field}{suffix}", dst.0, base.0)
        }
        Op::IndexAddr { dst, base, index } => {
            format!("v{} = index-addr v{} v{}", dst.0, base.0, index.0)
        }
        Op::Index { dst, base, index } => {
            format!("v{} = index v{} v{}", dst.0, base.0, index.0)
        }
        Op::Lookup {
            dst,
            map,
            key,
            comma_ok,
        } => format!("v{} = lookup v{} v{} ok={comma_ok}", dst.0, map.0, key.0),
        Op::Slice {
            dst,
            base,
            low,
            high,
            max,
        } => format!(
            "v{} = slice v{} [{}:{}:{}]",
            dst.0,
            base.0,
            low.map_or(String::new(), |v| format!("v{}", v.0)),
            high.map_or(String::new(), |v| format!("v{}", v.0)),
            max.map_or(String::new(), |v| format!("v{}", v.0))
        ),
        Op::BinOp {
            dst,
            kind,
            lhs,
            rhs,
        } => format!("v{} = binop {kind:?} v{} v{}", dst.0, lhs.0, rhs.0),
        Op::UnOp { dst, kind, operand } => format!("v{} = unop {kind:?} v{}", dst.0, operand.0),
        Op::Convert { dst, src } => format!("v{} = convert v{}", dst.0, src.0),
        Op::Extract { dst, tuple, index } => {
            format!("v{} = extract v{} #{index}", dst.0, tuple.0)
        }
        Op::Phi { dst, edges } => format!("v{} = phi {}", dst.0, vlist(edges)),
        Op::Call { dst, callee, args } => match dst {
            Some(d) => format!("v{} = {}({})", d.0, render_callee(p, callee), vlist(args)),
            None => format!("{}({})", render_callee(p, callee), vlist(args)),
        },
        Op::MakeClosure {
            dst,
            func,
            bindings,
        } => format!(
            "v{} = make-closure {} [{}]",
            dst.0,
            p.func_name(*func),
            vlist(bindings)
        ),
        Op::MakeInterface { dst, src } => format!("v{} = make-interface v{}", dst.0, src.0),
        Op::Make { dst, kind, args } => format!("v{} = make {kind:?} {}", dst.0, vlist(args)),
        Op::Send { chan, val } => format!("send v{} <- v{}", chan.0, val.0),
        Op::Recv {
            dst,
            chan,
            comma_ok,
        } => format!("v{} = recv v{} ok={comma_ok}", dst.0, chan.0),
        Op::CloseChan { chan } => format!("close v{}", chan.0),
        Op::Select {
            dst,
            arms,
            blocking,
        } => format!(
            "v{} = select blocking={blocking} [{}]",
            dst.0,
            arms.iter()
                .map(|a| match a.send {
                    Some(sv) => format!("send v{} <- v{}", a.chan.0, sv.0),
                    None => format!("recv v{}", a.chan.0),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Op::Go { callee, args } => format!("go {}({})", render_callee(p, callee), vlist(args)),
        Op::Defer { callee, args } => {
            format!("defer {}({})", render_callee(p, callee), vlist(args))
        }
        Op::Return { vals } => format!("return {}", vlist(vals)),
        Op::Jump => "jump".to_string(),
        Op::Branch { cond } => format!("branch v{}", cond.0),
        Op::Panic { val } => format!("panic v{}", val.0),
        Op::TypeAssert {
            dst,
            src,
            asserted,
            comma_ok,
        } => format!(
            "v{} = type-assert v{} {} ok={comma_ok}",
            dst.0,
            src.0,
            p.types().repr(*asserted)
        ),
        Op::Lock { kind, mu } => format!("{kind:?} v{}", mu.0).to_lowercase(),
        Op::Havoc { dst } => match dst {
            Some(d) => format!("v{} = havoc", d.0),
            None => "havoc".to_string(),
        },
    }
}
