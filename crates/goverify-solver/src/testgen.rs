//! Shared term generators (moved out of `reader.rs`'s prop-test module,
//! phase-3 Task 7 adjudication): both the reader's round-trip property
//! suite and the differential harness's nightly sweep need the same
//! sort-directed `Term` generator family, so it lives here once instead
//! of being duplicated.
//!
//! Compiled under `cfg(test)` (this crate's own `cargo test`, no feature
//! needed — `[dev-dependencies] proptest` already covers that path) OR
//! under the `testgen` cargo feature (the `differential` integration
//! test, a separate crate target that can't see `cfg(test)` items and
//! instead depends on `goverify-solver` with `features = ["testgen"]`).
//!
//! Sort-directed generators: each function only ever produces terms of
//! its named sort, so combinators like `select` (needs an Array-sorted
//! first operand) or `bvadd` (needs matching BitVec operands) draw
//! straight from a strategy that's already the right sort — no
//! generate-then-filter-and-retry, which (tried first) compounds across
//! recursion into a reject rate no budget converges on in reasonable
//! time. Every theory (bool, bitvec, array, Ptr datatype) shows up, AND
//! every `parse_term` node kind is reachable: `eq`, `not`, `and`, `or`,
//! `=>`, `ite`, `bvult`/`bvadd`, `select`/`store`, `(_ is ptr-nil)`, a
//! constructor applied to non-empty args (`ptr-addr`), and a field
//! accessor (`ptr-addr-val`).

use proptest::prelude::*;
use proptest::strategy::{BoxedStrategy, Union};

use crate::sort::{Sort, ptr_datatype, ptr_sort};
use crate::term::{BvBinOp, BvCmpOp, Term, ptr_is_nil, ptr_nil};

const DEPTH: u32 = 3;

pub fn arb_bool(depth: u32) -> BoxedStrategy<Term> {
    let leaf = prop_oneof![
        any::<bool>().prop_map(Term::bool_lit),
        Just(Term::var("b", Sort::Bool)),
    ];
    if depth == 0 {
        return leaf.boxed();
    }
    let d = depth - 1;
    prop_oneof![
        3 => leaf,
        1 => (arb_bool(d), arb_bool(d)).prop_map(|(a, b)| Term::eq(a, b).unwrap()),
        1 => arb_bool(d).prop_map(|a| Term::not(a).unwrap()),
        1 => prop::collection::vec(arb_bool(d), 1..3)
            .prop_map(|ts| Term::and(ts).unwrap()),
        1 => prop::collection::vec(arb_bool(d), 1..3)
            .prop_map(|ts| Term::or(ts).unwrap()),
        1 => (arb_bool(d), arb_bool(d))
            .prop_map(|(a, b)| Term::implies(a, b).unwrap()),
        1 => (arb_bool(d), arb_bool(d), arb_bool(d))
            .prop_map(|(c, t, e)| Term::ite(c, t, e).unwrap()),
        1 => (arb_bv(d, 8), arb_bv(d, 8))
            .prop_map(|(a, b)| Term::bv_cmp(BvCmpOp::Ult, a, b).unwrap()),
        1 => arb_ptr(d).prop_map(|a| ptr_is_nil(a).unwrap()),
        1 => (arb_arr(d), arb_bv(d, 8)).prop_map(|(a, i)| Term::select(a, i).unwrap()),
    ]
    .boxed()
}

/// `width` is either 8 (the array/comparison/general-purpose BitVec sort
/// used everywhere else) or 64 (only ever needed for the `ptr-addr`
/// field, `ptr-addr-val`); kept as one parameterized generator, rather
/// than a near-duplicate second function, so the `ptr-addr-val`
/// accessor path is visibly "arb_bv at width 64" and not a special case
/// bolted on elsewhere.
pub fn arb_bv(depth: u32, width: u32) -> BoxedStrategy<Term> {
    let max: u128 = if width >= 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    };
    let lit = (0..=max).prop_map(move |v| Term::bv_lit(width, v));
    let leaf = if width == 8 {
        prop_oneof![lit, Just(Term::var("x", Sort::BitVec(8)))].boxed()
    } else {
        lit.boxed()
    };
    if depth == 0 {
        return leaf;
    }
    let d = depth - 1;
    let mut arms = vec![
        (3, leaf),
        (
            1,
            (arb_bv(d, width), arb_bv(d, width))
                .prop_map(move |(a, b)| Term::bv_bin(BvBinOp::Add, a, b).unwrap())
                .boxed(),
        ),
        (
            1,
            (arb_bool(d), arb_bv(d, width), arb_bv(d, width))
                .prop_map(|(c, t, e)| Term::ite(c, t, e).unwrap())
                .boxed(),
        ),
    ];
    if width == 64 {
        // The only BitVec(64)-sorted term in v1's theories besides a
        // literal: read the address back out of a Ptr via the
        // `ptr-addr-val` accessor. `dt_get` only checks that its
        // argument has Ptr sort — it doesn't care whether that Ptr was
        // actually built via the `ptr-addr` constructor — so this is
        // always well-sorted for any `arb_ptr` output.
        arms.push((
            1,
            arb_ptr(d)
                .prop_map(|p| Term::dt_get(&ptr_datatype(), "ptr-addr", "ptr-addr-val", p).unwrap())
                .boxed(),
        ));
    }
    Union::new_weighted(arms).boxed()
}

pub fn arb_ptr(depth: u32) -> BoxedStrategy<Term> {
    let leaf = prop_oneof![Just(Term::var("p0", ptr_sort())), Just(ptr_nil())];
    if depth == 0 {
        return leaf.boxed();
    }
    let d = depth - 1;
    prop_oneof![
        2 => leaf,
        // Constructor applied to non-empty args: `(ptr-addr <bv64>)`.
        1 => arb_bv(d, 64).prop_map(|addr| {
            Term::dt_ctor(&ptr_datatype(), "ptr-addr", vec![addr]).unwrap()
        }),
        1 => (arb_bool(d), arb_ptr(d), arb_ptr(d))
            .prop_map(|(c, t, e)| Term::ite(c, t, e).unwrap()),
    ]
    .boxed()
}

pub fn arb_arr(depth: u32) -> BoxedStrategy<Term> {
    let leaf = Just(Term::var(
        "m",
        Sort::Array(Box::new(Sort::BitVec(8)), Box::new(Sort::Bool)),
    ));
    if depth == 0 {
        return leaf.boxed();
    }
    let d = depth - 1;
    prop_oneof![
        2 => leaf,
        1 => (arb_arr(d), arb_bv(d, 8), arb_bool(d))
            .prop_map(|(a, i, v)| Term::store(a, i, v).unwrap()),
        1 => (arb_bool(d), arb_arr(d), arb_arr(d))
            .prop_map(|(c, t, e)| Term::ite(c, t, e).unwrap()),
    ]
    .boxed()
}

/// Small random term over a fixed variable pool — every theory shows up:
/// bool, bitvec, arrays, the Ptr datatype.
pub fn arb_term() -> impl Strategy<Value = Term> {
    prop_oneof![
        arb_bool(DEPTH),
        arb_bv(DEPTH, 8),
        arb_ptr(DEPTH),
        arb_arr(DEPTH)
    ]
}

/// `arb_term()` coerced to always be Bool-sorted (`eq(t, t)` when it
/// isn't already): the differential harness only ever asserts Bool
/// terms, same coercion `reader.rs`'s round-trip property applies
/// inline.
pub fn arb_bool_term() -> BoxedStrategy<Term> {
    arb_term()
        .prop_map(|t| {
            if t.sort() == &Sort::Bool {
                t
            } else {
                Term::eq(t.clone(), t).unwrap()
            }
        })
        .boxed()
}
