//! Function summaries (parent spec §5), phase-2 form: clause structure and
//! instantiation are real; formulas are placeholders until phase 3's term
//! language replaces `PlaceholderFormula` behind this same API.

use goverify_ir::ValueId;

use crate::effects::Effects;

/// A variable of the function's symbolic interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IfaceVar {
    Param(u32),
    Result(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceholderFormula {
    /// Which fact this clause states, e.g. "nonnil". Opaque to phase 2.
    pub tag: String,
    pub vars: Vec<IfaceVar>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clause {
    pub formula: PlaceholderFormula,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    Inferred,
    Havoc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub requires: Vec<Clause>,
    pub ensures: Vec<Clause>,
    pub effects: Effects,
    pub provenance: Provenance,
}

impl Default for Summary {
    fn default() -> Self {
        Summary {
            requires: Vec::new(),
            ensures: Vec::new(),
            effects: Effects::empty(),
            provenance: Provenance::Inferred,
        }
    }
}

impl Summary {
    /// The unknown-function summary: no requires (missing info must never
    /// create false positives), top effects (assume the worst).
    pub fn havoc() -> Summary {
        Summary {
            requires: Vec::new(),
            ensures: Vec::new(),
            effects: Effects::top(),
            provenance: Provenance::Havoc,
        }
    }
}

/// A callee clause bound to caller values. None = the interface var had no
/// corresponding caller value (malformed input or Result var) — callers
/// must treat None as "cannot evaluate; do not report".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundClause {
    pub tag: String,
    pub vars: Vec<Option<ValueId>>,
}

pub fn instantiate_requires(callee: &Summary, args: &[ValueId]) -> Vec<BoundClause> {
    callee
        .requires
        .iter()
        .map(|c| BoundClause {
            tag: c.formula.tag.clone(),
            vars: c
                .formula
                .vars
                .iter()
                .map(|v| match v {
                    IfaceVar::Param(i) => args.get(*i as usize).copied(),
                    IfaceVar::Result(_) => None,
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instantiate_maps_params_to_args() {
        let callee = Summary {
            requires: vec![Clause {
                formula: PlaceholderFormula {
                    tag: "nonnil".into(),
                    vars: vec![IfaceVar::Param(0), IfaceVar::Param(2)],
                },
            }],
            ..Summary::default()
        };
        let args = [ValueId(7), ValueId(8), ValueId(9)];
        let bound = instantiate_requires(&callee, &args);
        assert_eq!(
            bound,
            vec![BoundClause {
                tag: "nonnil".into(),
                vars: vec![Some(ValueId(7)), Some(ValueId(9))],
            }]
        );
    }

    #[test]
    fn instantiate_out_of_range_param_binds_none() {
        let callee = Summary {
            requires: vec![Clause {
                formula: PlaceholderFormula {
                    tag: "t".into(),
                    vars: vec![IfaceVar::Param(5)],
                },
            }],
            ..Summary::default()
        };
        assert_eq!(instantiate_requires(&callee, &[])[0].vars, vec![None]);
    }

    #[test]
    fn havoc_summary_has_no_requires() {
        // Missing info must never create false positives (parent spec §11).
        let h = Summary::havoc();
        assert!(h.requires.is_empty());
        assert_eq!(h.provenance, Provenance::Havoc);
        assert_eq!(h.effects, crate::effects::Effects::top());
    }
}
