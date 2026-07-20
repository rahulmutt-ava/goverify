// Package nilcorpus exercises the phase-4 nil checker: guards as path
// conditions, requires propagation, manifest-local nils, loops.
package nilcorpus

type T struct{ X int }

// deref unconditionally dereferences p: requires p != nil.
func deref(p *T) int { return p.X }

// guarded checks first — no requires, no finding.
func guarded(p *T) int {
	if p == nil {
		return 0
	}
	return p.X
}

// wrapper passes its own param through: inherits deref's requires,
// reports nothing itself.
func wrapper(p *T) int { return deref(p) }

// Bad passes a constant nil to deref.
func Bad() int { return deref(nil) } // want: nil-deref

// BadTwoHops trips wrapper's INHERITED requires.
func BadTwoHops() int { return wrapper(nil) } // want: nil-deref

// LocalNil dereferences a manifest local nil.
func LocalNil() int {
	var p *T
	return p.X // want: nil-deref
}

// BadMethod dereferences a manifest local nil inside a pointer-receiver
// method, so its finding carries a go/ssa METHOD id
// — (*example.com/nil.T).BadMethod — which the check scoping filter must
// still recognize as in-module (final review F1 fix 2).
func (t *T) BadMethod() int {
	var p *T
	return p.X // want: nil-deref
}

// Good passes nil only to the guarded function.
func Good() int { return guarded(nil) }

// LoopGuarded derefs only after a loop that assigns p — the back-edge
// cut keeps the first-iteration path; p is non-nil on every kept path.
func LoopGuarded(ts []*T) int {
	total := 0
	for _, t := range ts {
		if t == nil {
			continue
		}
		total += t.X
	}
	return total
}

// --- fix-wave fix 2a: same-function dominating check carried forward ---

type inner struct{ n int }

type holder struct{ cached *inner }

func use(i *inner) int { return i.n } // infers requires: i != nil

func observe() {}

// RecheckedField mirrors bbolt C015a: the nil-check dominates the use,
// and the intervening call must not invalidate the forwarded load of
// h.cached. Green: no finding.
func RecheckedField(h *holder) int {
	if h.cached == nil {
		return 0
	}
	observe()
	return use(h.cached)
}

// StoreInvalidates: a store to the checked field between check and use
// makes the re-read genuinely unconstrained again — must still report.
func StoreInvalidates(h *holder, fresh *inner) int {
	if h.cached == nil {
		return 0
	}
	h.cached = fresh
	return use(h.cached) // want: nil-deref
}

// --- fix-wave fix 2b: checked-deref (not just nil-check) dominance ---

// DerefThenCall (fix 2b): the field read itself — not a nil-check —
// dominates the call; reaching the call means the deref succeeded, so
// the callee's non-nil requirement is met. Green: no finding.
func DerefThenCall(h *holder) int {
	n := h.cached.n
	return n + use(h.cached)
}

// CallThenDeref (fix 2b red): the call precedes any deref — nothing
// dominates it, the obligation must survive.
func CallThenDeref(h *holder) int {
	n := use(h.cached) // want: nil-deref
	return n + h.cached.n
}

// --- fix-wave fix 5: nil-map range raises no obligation ---

// RangeNilMap (fix 5): ranging over a nil map is legal Go — zero
// iterations, no dereference. The range header must not report.
// (Dereferencing f itself infers a requires clause on f, which is a
// summary, not a finding.)
type freeMaps struct{ maps map[uint64]bool }

func RangeNilMap(f *freeMaps) int {
	n := 0
	for k := range f.maps {
		if k > 0 {
			n++
		}
	}
	return n
}

// RangeNilMapCaller (fix 5 red): the receiver deref (f.maps reads f)
// is real — RangeNilMap's inferred requires must still propagate, so
// a nil argument still reports at the call site.
func RangeNilMapCaller() int { return RangeNilMap(nil) } // want: nil-deref

// --- fix-wave fix 2b: cross-block dominance ---

// DerefInDominatorThenBranchCall (fix 2b, cross-block green): the deref
// of h.cached sits in a block that strictly dominates the post-join
// call — every path to the call passes the deref. No finding.
func DerefInDominatorThenBranchCall(h *holder, flip bool) int {
	n := h.cached.n
	if flip {
		n++
	} else {
		n--
	}
	return n + use(h.cached)
}

// BranchDerefJoinCall (fix 2b, cross-block red): the deref happens in
// only ONE branch arm — it does not dominate the join, so the call's
// obligation must survive.
func BranchDerefJoinCall(h *holder, flip bool) int {
	n := 0
	if flip {
		n = h.cached.n
	}
	return n + use(h.cached) // want: nil-deref
}

// --- interprocedural summaries: obligations on call-result subjects ---

type Bucket struct{ Fill int }

type createError struct{}

func (e *createError) Error() string { return "create failed" }

var errCreate = &createError{}

// createBucket carries the inferred correlation err==nil ⇒ result!=nil
// (Go-idiom rule: the failure site returns a sentinel).
func createBucket(fail bool) (*Bucket, error) {
	if fail {
		return nil, errCreate
	}
	return &Bucket{}, nil
}

// IgnoredErr discards the error and dereferences the result — the
// FillPercent restoration shape (bbolt cmd/bbolt/main.go:1191): the
// discarded error leaves the correlation unbindable, the result stays
// possibly-nil, and the deref must now be flagged at the true
// first-failure site.
func IgnoredErr(fail bool) int {
	b, _ := createBucket(fail)
	return b.Fill // want: nil-deref
}

// GuardedErr checks the error first: the asserted correlation plus the
// guard discharge the deref. No finding.
func GuardedErr(fail bool) int {
	b, err := createBucket(fail)
	if err != nil {
		return 0
	}
	return b.Fill
}

// fresh always allocates: the unconditional ensures discharges the
// unguarded deref of its result. No finding.
func fresh() *Bucket { return &Bucket{} }

func UnguardedFresh() int { return fresh().Fill }

// --- interprocedural summaries: ChangeType/Assign canonicalization ---

type NamedPtr *T

// chained reaches p only through ChangeType copies (each lowers to an
// SSA Assign). The deref subject must canonicalize back to p so
// chained emits its own ¬nil(p) requires — previously the copy's
// checked-deref assumption silently discharged p without any caller
// ever being told (the documented Assign/ChangeType composition FN).
func chained(p *T) int {
	q := NamedPtr(p)
	r := (*T)(q)
	return r.X
}

func BadChained() int { return chained(nil) } // want: nil-deref
