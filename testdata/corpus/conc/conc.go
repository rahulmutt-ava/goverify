package conc

import "sync"

type Closer interface{ Close() error }

// File is exported so `go/ssa/ssautil.AllFunctions` treats its methods as
// roots (it only does so for methods of exported, non-parameterized
// types); otherwise `(*File).Close`, reached only through CloseAll's
// invoke-mode dispatch and never via a direct SSA value, would never be
// materialized with a body in the extracted .gvir at all.
type File struct{ mu sync.Mutex }

func (f *File) Close() error {
	f.mu.Lock()
	defer f.mu.Unlock()
	return nil
}

func CloseAll(cs []Closer) {
	for _, c := range cs {
		_ = c.Close() // invoke-mode call
	}
}

func Fan(a, b chan int) int {
	done := make(chan struct{})
	go func() { close(done) }() // go + closure + builtin close
	select {
	case v := <-a:
		return v
	case b <- 1:
		return 1
	case <-done:
		return 0
	}
}

// Renamer/Thing/RenameAll regression (final-review C1): the interface
// declares NAMED params/results; Thing's own implementation uses a
// DIFFERENTLY-named param and unnamed results — the norm for real Go
// (io.Writer's `Write(p []byte) (n int, err error)` vs almost every
// concrete Write method). `emit.go` interns a signature's TypeId by its
// full repr string (parameter names included), so the interface's
// declared signature and Thing's own declared signature land on
// different TypeIds despite being structurally identical; a
// signature-matcher keyed on the raw TypeId must not let that drop the
// invoke edge.
type Renamer interface {
	Rename(newName string) (ok bool, err error)
}

// Thing is exported for the same reason File is (see above): so
// ssautil.AllFunctions roots (*Thing).Rename, which is otherwise reached
// only through RenameAll's invoke-mode dispatch.
type Thing struct{}

func (Thing) Rename(n string) (bool, error) { return true, nil }

func RenameAll(rs []Renamer) {
	for _, r := range rs {
		_, _ = r.Rename("x") // invoke-mode call
	}
}

// InvokeCB/NamedParamImpl/UseNamedParamImpl regression (final-review
// C1): a dynamic call through a function-typed parameter whose declared
// signature names its parameter differently than the function value
// passed to it — same root cause as above, exercised through
// address-taken/Callee::Dynamic resolution instead of invoke-mode.
func InvokeCB(cb func(x string) int) int {
	return cb("hi")
}

func NamedParamImpl(m string) int { return len(m) }

func UseNamedParamImpl() int {
	return InvokeCB(NamedParamImpl)
}

// DeferClose/GoClose regression (final-review C2): `defer close(ch)` and
// `go close(ch)` don't go through lower.rs's plain-call intrinsic
// rewrite (that rewrite only applies to a plain, non-deferred,
// non-goroutine `Op::Call`), so they stay `Op::Defer`/`Op::Go` with a
// `Callee::Builtin("close")` callee. Unlike `Fan`'s `go func(){
// close(done) }()` above (where `close` is a plain call *inside* the
// closure body, already covered by the plain-call rewrite), these call
// `close` directly as the deferred/spawned operation itself — the exact
// shape effects::collect must still record as ChanOp::Close.
func DeferClose(ch chan int) {
	defer close(ch)
}

func GoClose(ch chan int) {
	go close(ch)
}
