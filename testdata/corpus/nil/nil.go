// Package nilcorpus exercises the phase-3 nil tracer end to end:
// one inferred requires, one violated call site, one guarded function.
package nilcorpus

type T struct{ X int }

// deref unconditionally dereferences p in its entry block: the tracer
// must infer `requires p != nil`.
func deref(p *T) int { return p.X }

// guarded checks first — deref happens in a non-entry block; the
// entry-block tracer must infer nothing. // want: no finding
func guarded(p *T) int {
	if p == nil {
		return 0
	}
	return p.X
}

// Bad passes a constant nil to deref. // want: nil finding here
func Bad() int { return deref(nil) }

// Good passes nil only to the guarded function. // want: no finding
func Good() int { return guarded(nil) }
