//! Immediate dominators over the encoder's cut DAG (fix-wave fix 2b):
//! Cooper–Harvey–Kennedy, one pass in topological order (sufficient on
//! an acyclic graph — every pred is finalized before its successors).

use std::collections::BTreeMap;

pub(crate) use crate::encode::topo_order;

/// idom per block over the cut DAG. `idom[0] = Some(0)` (the entry is
/// its own root); `None` = unreachable from the entry. One pass in
/// topological order: acyclic graph, so every processed pred is final.
pub fn dominators(dag_succs: &[Vec<u32>]) -> Vec<Option<usize>> {
    let n = dag_succs.len();
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, ss) in dag_succs.iter().enumerate() {
        for &s in ss {
            if (s as usize) < n {
                preds[s as usize].push(b);
            }
        }
    }
    let order = topo_order(dag_succs);
    let pos: BTreeMap<usize, usize> = order.iter().enumerate().map(|(i, &b)| (b, i)).collect();
    let mut idom: Vec<Option<usize>> = vec![None; n];
    if n == 0 {
        return idom;
    }
    idom[0] = Some(0);
    for &b in order.iter().skip(1) {
        let mut new: Option<usize> = None;
        for &pd in &preds[b] {
            if idom[pd].is_none() {
                continue; // unreachable pred contributes nothing
            }
            new = Some(match new {
                None => pd,
                Some(cur) => intersect(&idom, &pos, cur, pd),
            });
        }
        idom[b] = new;
    }
    idom
}

/// CHK two-finger intersection walking idom chains by topo position.
fn intersect(
    idom: &[Option<usize>],
    pos: &BTreeMap<usize, usize>,
    mut a: usize,
    mut b: usize,
) -> usize {
    while a != b {
        let (Some(&pa), Some(&pb)) = (pos.get(&a), pos.get(&b)) else {
            return 0; // degraded input: fall back to the entry
        };
        if pa > pb {
            a = idom[a].unwrap_or(0);
        } else {
            b = idom[b].unwrap_or(0);
        }
    }
    a
}

/// True iff `a` strictly dominates `b`: a != b and a is on b's idom
/// chain. Walks at most n links (idom chains are acyclic toward the
/// entry); any degraded/None link means "don't know" = false.
pub fn strictly_dominates(idom: &[Option<usize>], a: usize, b: usize) -> bool {
    if a == b {
        return false;
    }
    let mut cur = b;
    for _ in 0..idom.len() {
        match idom.get(cur).copied().flatten() {
            Some(d) if d == a => return true,
            Some(d) if d == cur => return false, // reached the entry
            Some(d) => cur = d,
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diamond_joins_at_entry() {
        // 0 -> {1,2} -> 3: idom(3) = 0; 1 and 2 do NOT dominate 3.
        let dag = vec![vec![1, 2], vec![3], vec![3], vec![]];
        let idom = dominators(&dag);
        assert_eq!(idom, vec![Some(0), Some(0), Some(0), Some(0)]);
        assert!(strictly_dominates(&idom, 0, 3));
        assert!(!strictly_dominates(&idom, 1, 3));
        assert!(!strictly_dominates(&idom, 2, 3));
        assert!(!strictly_dominates(&idom, 3, 3), "strict: never self");
    }

    #[test]
    fn chain_dominates_transitively() {
        // 0 -> 1 -> 2: both 0 and 1 strictly dominate 2.
        let dag = vec![vec![1], vec![2], vec![]];
        let idom = dominators(&dag);
        assert_eq!(idom, vec![Some(0), Some(0), Some(1)]);
        assert!(strictly_dominates(&idom, 0, 2));
        assert!(strictly_dominates(&idom, 1, 2));
    }

    #[test]
    fn unreachable_block_has_no_idom() {
        // 0 -> 1; 2 floats (unreachable — e.g. an all-in-edges-cut loop).
        let dag = vec![vec![1], vec![], vec![1]];
        let idom = dominators(&dag);
        assert_eq!(idom[2], None);
        assert!(
            !strictly_dominates(&idom, 2, 1),
            "unreachable dominates nothing"
        );
    }
}
