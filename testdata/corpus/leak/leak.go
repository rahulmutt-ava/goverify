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

// Reported (one hop, plain call): the bbolt (*Tx).check shape — every
// send lives in a helper the goroutine calls.
func LeakHelperSend() {
	ch := make(chan int)
	go spawnHelperSend(ch) // want: chan-send-leak
}

func spawnHelperSend(c chan int) { helperSend(c) }

func helperSend(c chan int) { c <- 1 }

// Reported (one hop, deferred call): the errgroup (*Group).done shape —
// the recv is reached through a defer in the spawned callee.
func LeakDeferHelperRecv() {
	ch := make(chan int)
	go spawnDeferRecv(ch) // want: chan-recv-leak
}

func spawnDeferRecv(c chan int) { defer helperRecv(c) }

func helperRecv(c chan int) { <-c }

// Reported (one hop, deferred closure): the singleflight doCall$1
// shape — the send lives in a closure the spawned callee defers; the
// captured param spills to a cell the hop mapping's single-store
// bridge resolves.
func LeakDeferClosureSend() {
	ch := make(chan int)
	go spawnDeferClosure(ch) // want: chan-send-leak
}

func spawnDeferClosure(c chan int) {
	defer func() { c <- 1 }()
}

// Silent: the hop candidate's counterpart recv exists in the spawner
// (rule 3 consults summarized helper ops — nothing hop-specific).
func NoLeakHelperPaired() int {
	ch := make(chan int)
	go spawnHelperSend(ch)
	return <-ch
}

// Silent (v1 boundary pin — a REAL leak the one-hop rule cannot see):
// the blocking send is two calls below the spawned callee. Tripwire
// for any future depth change (spec §10 "depth ≥ 2 anchoring").
func SilentHelperDepth2() {
	ch := make(chan int)
	go depth2Spawnee(ch)
}

func depth2Spawnee(c chan int) { depth2Mid(c) }

func depth2Mid(c chan int) { helperSend(c) }

// Silent (v1 boundary pin — a REAL leak, documented): a buffered-const
// send via a hop never gets the ordinal fill-count argument (spec
// §5.1), so the second helper call's send — genuinely blocked, cap 1,
// two sends, no drain — stays silent.
func SilentHelperBuffered() {
	ch := make(chan int, 1)
	go bufSpawnee(ch)
}

func bufSpawnee(c chan int) {
	bufHelper(c)
	bufHelper(c)
}

func bufHelper(c chan int) { c <- 1 }
