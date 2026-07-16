package main

import (
	"strings"
	"testing"

	"golang.org/x/tools/go/packages"
)

// TestRelPathGoroot pins F3: relPath must consult e.goroot (resolved by
// the caller via `go env GOROOT`), never runtime.GOROOT(). With an empty
// goroot (as happens when the resolution step is skipped or fails) it
// must degrade to the basename, never emit a "$GOROOT/" prefix; with a
// goroot set it must rewrite stdlib-looking paths beneath it.
func TestRelPathGoroot(t *testing.T) {
	pkg := &packages.Package{} // no Module: exercises the goroot branch

	t.Run("empty goroot never produces a $GOROOT/ prefix", func(t *testing.T) {
		e := &emitter{pkg: pkg, goroot: ""}
		got := e.relPath("/usr/local/go/src/fmt/print.go")
		if strings.HasPrefix(got, "$GOROOT/") {
			t.Errorf("relPath(%q) = %q, want no $GOROOT/ prefix when goroot is unresolved", "/usr/local/go/src/fmt/print.go", got)
		}
		if got != "print.go" {
			t.Errorf("relPath(%q) = %q, want basename fallback %q", "/usr/local/go/src/fmt/print.go", got, "print.go")
		}
	})

	t.Run("set goroot rewrites to $GOROOT-relative", func(t *testing.T) {
		e := &emitter{pkg: pkg, goroot: "/usr/local/go"}
		got := e.relPath("/usr/local/go/src/fmt/print.go")
		want := "$GOROOT/src/fmt/print.go"
		if got != want {
			t.Errorf("relPath(%q) = %q, want %q", "/usr/local/go/src/fmt/print.go", got, want)
		}
	})
}
