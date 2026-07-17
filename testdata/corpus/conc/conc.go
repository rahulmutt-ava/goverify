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
