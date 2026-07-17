package main

import (
	"strings"
	"testing"

	"golang.org/x/tools/go/packages"

	"goverify.dev/extractor/gvirpb"
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

func TestStructuredTypes(t *testing.T) {
	pkgs := extractCorpus(t, "../testdata/corpus/hello", false)
	p := pkgs["example.com/hello"]
	byRepr := map[string]*gvirpb.Type{}
	for _, ty := range p.Types {
		byRepr[ty.Repr] = ty
		if ty.Kind == gvirpb.TypeKind_TYPE_KIND_UNSPECIFIED {
			t.Errorf("type %q has unspecified kind", ty.Repr)
		}
	}
	intT, ok := byRepr["int"]
	if !ok {
		t.Fatal("no int type interned")
	}
	if intT.Kind != gvirpb.TypeKind_TYPE_KIND_BASIC || intT.Name != "int" {
		t.Errorf("int: kind=%v name=%q", intT.Kind, intT.Name)
	}
}

func TestStructuredTypesRecursive(t *testing.T) {
	// withdeps or a dedicated fixture must contain: type node struct{ next *node }
	pkgs := extractCorpus(t, "../testdata/corpus/withdeps", false)
	p := pkgs["example.com/withdeps"]
	var structT *gvirpb.Type
	for _, ty := range p.Types {
		if ty.Kind == gvirpb.TypeKind_TYPE_KIND_STRUCT && len(ty.Fields) == 1 && ty.Fields[0].Name == "next" {
			structT = ty
		}
	}
	if structT == nil {
		t.Fatal("recursive struct not found (add `type node struct{ next *node }` + use to withdeps)")
	}
	ptr := p.Types[structT.Fields[0].Type-1] // ids are 1-based, table sorted by id
	if ptr.Kind != gvirpb.TypeKind_TYPE_KIND_POINTER {
		t.Errorf("next field: want pointer, got %v", ptr.Kind)
	}
}

// findInstr returns instructions of the given kind across all functions.
func findInstr(p *gvirpb.Package, kind string) []*gvirpb.Instruction {
	var out []*gvirpb.Instruction
	for _, f := range p.Functions {
		for _, b := range f.Blocks {
			for _, ins := range b.Instrs {
				if ins.Kind == kind {
					out = append(out, ins)
				}
			}
		}
	}
	return out
}

func TestStructuredConstsAndSems(t *testing.T) {
	pkgs := extractCorpus(t, "../testdata/corpus/hello", false)
	p := pkgs["example.com/hello"]

	// hello.Add contains a BinOp; its sem must carry the token.
	binops := findInstr(p, "BinOp")
	if len(binops) == 0 {
		t.Fatal("no BinOp in hello corpus")
	}
	for _, ins := range binops {
		if ins.GetBinop().GetOp() == "" {
			t.Errorf("BinOp without sem.op: %s", ins.Detail)
		}
	}

	// Every Const aux value must carry a structured ConstValue.
	for _, f := range p.Functions {
		for _, a := range f.Aux {
			if a.Kind == "Const" && a.Const == nil {
				t.Errorf("%s: const aux %q lacks ConstValue", f.Id, a.Repr)
			}
		}
	}
}
