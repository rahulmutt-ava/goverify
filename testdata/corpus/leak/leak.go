package leak

// Reported: unbuffered send, closure capture (THE canonical shape).
func LeakSendClosure() {
	ch := make(chan int)
	go func() { ch <- 1 }() // want: chan-send-leak
	_ = ch
}

// Reported: unbuffered send, channel passed as go-arg to a named fn.
func LeakSendParam() {
	ch := make(chan int)
	go produce(ch) // want: chan-send-leak
}

func produce(c chan int) { c <- 1 }

// Reported: recv with no sender and no close.
func LeakRecvClosure() {
	ch := make(chan int)
	go func() { <-ch }() // want: chan-recv-leak
}

// Reported: cyclic producer on an unbuffered channel (first send blocks;
// no counting needed).
func LeakLoopProducer(xs []int) {
	ch := make(chan int)
	go func() { // want: chan-send-leak
		for _, x := range xs {
			ch <- x
		}
	}()
}

// Reported: buffered, const cap 1, second acyclic send overflows.
func LeakBufferedOverflow() {
	ch := make(chan int, 1)
	go func() { // want: chan-send-leak
		ch <- 1
		ch <- 2
	}()
}

// Silent: counterpart recv exists in the spawner.
func NoLeakPaired() int {
	ch := make(chan int)
	go func() { ch <- 1 }()
	return <-ch
}

// Silent: buffered send fits the buffer; goroutine terminates.
func NoLeakBufferedFits() {
	ch := make(chan int, 1)
	go func() { ch <- 1 }()
}

// Silent: close is a (conservative) counterpart for the blocked recv.
func NoLeakClosed() {
	ch := make(chan int)
	go func() { <-ch }()
	close(ch)
}

// Silent: param-rooted — the caller may hold receivers.
func NoLeakParamRooted(ch chan int) {
	go func() { ch <- 1 }()
}

// Silent: channel escapes via heap store.
var sink chan int

func NoLeakEscapeStore() {
	ch := make(chan int)
	sink = ch
	go func() { ch <- 1 }()
}

// Silent: channel escapes via return.
func NoLeakEscapeReturn() chan int {
	ch := make(chan int)
	go func() { ch <- 1 }()
	return ch
}

// Silent: channel passed to a plain call (strict arg-escape rule).
func NoLeakEscapeCall() {
	ch := make(chan int)
	drain(ch)
	go func() { ch <- 1 }()
}

func drain(c chan int) { go func() { <-c }() }

// Silent: global-rooted channel — any package can hold a counterpart
// (the candidate root is Global, not Alloc-in-spawner).
var gch = make(chan int)

func NoLeakGlobalRooted() {
	go func() { gch <- 1 }()
}

// Reported: a GENUINELY-unresolved dynamic call in the goroutine does
// NOT suppress. `hook`'s signature (func(int) int) matches no
// address-taken function anywhere else in this module (every other
// closure here is func()), so the call-graph's structural-key dynamic
// dispatch resolves zero targets for it — it contributes no effects, and
// the escape walk proves the channel can never reach it, so no one can
// unblock the send. (A SAME-signature dynamic call is a different,
// documented v1 FN surface: the call-graph's may-call resolution over
// address-taken functions is conservative CHA-style over-approximation —
// any two same-signature closures in one module become each other's
// possible dynamic targets, so a matching-signature opaque call CAN
// launder Unknown-bucket effects and suppress a real leak. Task 10
// records this in spec §8; a BODYLESS static callee's own top() effects
// are pinned by the effects/has_counterpart unit tests instead.)
var hook func(int) int

func LeakDespiteOpaqueCall() {
	ch := make(chan int)
	go func() { // want: chan-send-leak
		_ = hook(1)
		ch <- 1
	}()
}

// Silent: select with default never blocks.
func NoLeakSelectDefault() {
	ch := make(chan int)
	go func() {
		select {
		case ch <- 1:
		default:
		}
	}()
}

// Reported: blocking select, all arms dead.
func LeakSelectAllBlocked() {
	a := make(chan int)
	b := make(chan int)
	go func() { // want: chan-select-leak
		select {
		case <-a:
		case b <- 1:
		}
	}()
}
