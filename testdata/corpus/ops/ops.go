// Package ops exercises every value-op family for lowering goldens.
package ops

type pair struct {
	a, b int
	next *pair
}

func Deref(p *pair) int                           { return p.a }   // load, field-addr
func StoreIt(p *pair, v int)                      { p.b = v }      // store
func Idx(xs []int, i int) int                     { return xs[i] } // index-addr, load
func arr4() [4]int                                { return [4]int{1, 2, 3, 4} }
func ArrIdx(i int) int                            { return arr4()[i] }               // index (non-addressable array rvalue)
func Look(m map[string]int, k string) (int, bool) { v, ok := m[k]; return v, ok }    // lookup comma-ok
func Sl(xs []int) []int                           { return xs[1:3] }                 // slice
func Arith(a, b int) int                          { return a / b }                   // binop div
func Narrow(x int64) int8                         { return int8(x) }                 // convert narrowing
func Assert(v any) (int, bool)                    { n, ok := v.(int); return n, ok } // type-assert
func Closure(x int) func() int                    { return func() int { return x } } // make-closure, freevar
func Iface(v any) any                             { return v }
func IfaceCaller() any                            { return Iface(7) } // make-interface at call site
func Ptr() *pair                                  { return &pair{} }  // alloc heap
func Loop(xs []int) int { // phi, branch, jump
	s := 0
	for _, x := range xs {
		s += x
	}
	return s
}
func Panics(v any)           { panic(v) }       // panic
func Variadic(xs ...int) int { return len(xs) } // builtin len
func RecvPlain(ch chan int) int { return <-ch } // unop recv (plain, no comma-ok)
