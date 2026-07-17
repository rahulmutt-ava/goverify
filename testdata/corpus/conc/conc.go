package conc

import "sync"

type Closer interface{ Close() error }

type file struct{ mu sync.Mutex }

func (f *file) Close() error {
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
