// Package hello is a goverify extraction corpus module: it exercises
// derefs, methods, closures, goroutines, channels, and pragmas.
package hello

//goverify:requires p != nil
func Deref(p *int) int { return *p }

func Add(a, b int) int { return a + b }

type Counter struct{ n int }

func (c *Counter) Inc() { c.n++ }

func Spawn(ch chan int) {
	go func() { ch <- 1 }()
}
