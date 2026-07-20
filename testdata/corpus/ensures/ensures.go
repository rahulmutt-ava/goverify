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
