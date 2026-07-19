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
