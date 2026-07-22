// Package ensures exercises postcondition (ensures) inference: the
// unconditional never-nil template and the (T, error) correlation,
// validated per return site with the Go-idiom rule for non-literal-nil
// error expressions (sentinel errors).
package ensures

type T struct{ X int }

type opError struct{}

func (e *opError) Error() string { return "op failed" }

// A package-level sentinel: loads of it are havoc'd, which is exactly
// why the correlation template needs the Go-idiom rule.
var errOp = &opError{}

// MakeT always returns a fresh allocation: unconditional ensures
// ¬is_nil(r0) must be proven (Alloc dsts are never nil).
func MakeT() *T { return &T{} }

// NewT is the canonical constructor shape: (nil, sentinel) on failure,
// (alloc, nil) on success. The correlation is_nil(r1) ⇒ ¬is_nil(r0)
// must be emitted: the failure site passes by the idiom rule (non-
// literal-nil error), the success site by SMT proof (alloc non-nil,
// error component is the nil literal).
func NewT(fail bool) (*T, error) {
	if fail {
		return nil, errOp
	}
	return &T{}, nil
}

// MayNil returns (nil, nil) on one path: NEITHER template may validate
// (the success-shaped site pairs a nil literal error with a nil
// result — the SMT check must reject).
func MayNil(b bool) (*T, error) {
	if b {
		return nil, nil
	}
	return &T{}, nil
}

// newA is a second NewT-shaped constructor so the dispatch wrapper
// below has two distinct callees (the DB.Begin shape).
func newA(fail bool) (*T, error) {
	if fail {
		return nil, errOp
	}
	return &T{}, nil
}

// NewTVia is a bare forwarding dispatch wrapper: each return site
// forwards a callee's whole tuple (`return f(...)`), which SSA lowers
// via per-component `Extract`s ahead of a component-wise `Return` — not
// a single tuple-valued Return operand (confirmed by direct IR
// inspection, task-1 investigation). The ensures template-2 Go-idiom
// rule accepts ANY non-literal error component at a return site, and
// an Extract is non-literal, so wrappers of idiomatic callees (newA,
// NewT: never (nil, nil)) keep the (T, error) correlation WITHOUT any
// consultation of the callee's summary (C009c hypothesis H1, arity
// form: refuted — this probe is a green regression guard, not a RED
// tripwire). Consequence: a wrapper of a NON-idiomatic callee (one
// that can return (nil, nil), e.g. MayNil) would receive this same
// correlation clause even though its callee can't support it — a
// pre-existing declared under-approximation of the Go-idiom rule,
// inherited from the summaries wave (tripwire pin queued as
// follow-up).
func NewTVia(fail, alt bool) (*T, error) {
	if alt {
		return newA(fail)
	}
	return NewT(fail)
}

// NewTNamed is the real DB.Begin shape: NAMED results plus a deferred
// closure reading err, which forces SSA to materialize named-result
// cells (returns become stores + a component-wise load-per-component
// Return, same as NewTVia's shape). Same mechanism as NewTVia: the
// Go-idiom site rule accepts extract-shaped (here, load-of-cell-
// shaped) error components, so this wrapper of idiomatic callees keeps
// the correlation too. Pins the second H1 form: also refuted, also
// green.
func NewTNamed(fail bool) (t *T, err error) {
	defer func() { _ = err }()
	if fail {
		return newA(fail)
	}
	return NewT(fail)
}

// MayNilVia is the laundering-boundary tripwire queued by the
// summaries follow-up wave (wave-2 spec §4): a bare forwarding wrapper
// of a NON-idiomatic callee. MayNil can return (nil, nil), so no sound
// (T, error) correlation exists for this wrapper — but the Go-idiom
// rule accepts the extract-shaped error component without consulting
// MayNil's summary and mints the clause anyway (same mechanism as
// NewTVia above, KNOWN false-discharge boundary, threat-model.md).
// The corpus pin asserts the laundered clause IS emitted, so any
// change to the Go-idiom rule flips the pin visibly instead of
// silently moving the soundness boundary.
func MayNilVia(b bool) (*T, error) {
	return MayNil(b)
}

// Rec2's recursion is irrelevant to its result: the single return
// site yields a fresh allocation, so the unconditional ensures must
// be inferred even though Rec2 forms a recursive SCC — pins the
// simultaneous-fixpoint soundness examined in the summaries wave's
// final review (self-consultation via the in-flight summary).
func Rec2(n int) *T {
	if n > 0 {
		_ = Rec2(n - 1)
	}
	return &T{}
}

// Rec forwards its own recursive result: the optimistic fixpoint
// starts it clause-free and nothing independent ever proves the
// candidate, so the converged summary must STAY clause-free — the
// inference must not bootstrap a self-justifying ensures.
func Rec(n int) *T {
	if n == 0 {
		return &T{}
	}
	return Rec(n - 1)
}

type Iface interface{ M() }

type impl struct{ x int }

func (i *impl) M() {}

// AsIface returns a typed-nil-prone interface: on the fail path the
// wrapped *impl is nil while the interface value itself is a
// MakeInterface product. Inference must NOT claim ¬is_nil(r0) —
// interfaces are Ptr-sorted since the summaries wave, and this pins
// the boundary against the Go-idiom under-approximation silently
// widening to interface results.
func AsIface(fail bool) Iface {
	var p *impl
	if !fail {
		p = &impl{}
	}
	return p
}
