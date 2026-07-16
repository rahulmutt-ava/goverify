package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"google.golang.org/protobuf/proto"

	"goverify.dev/extractor/gvirpb"
)

const helloDir = "../testdata/corpus/hello"

// extractCorpus runs the full extraction pipeline over a corpus module
// and decodes every emitted .gvir, keyed by import path.
func extractCorpus(t *testing.T, dir string, deps bool) map[string]*gvirpb.Package {
	t.Helper()
	out := t.TempDir()
	written, err := run(dir, []string{"./..."}, out, deps)
	if err != nil {
		t.Fatalf("run(%s): %v", dir, err)
	}
	pkgs := map[string]*gvirpb.Package{}
	for _, w := range written {
		raw, err := os.ReadFile(w)
		if err != nil {
			t.Fatal(err)
		}
		var p gvirpb.Package
		if err := proto.Unmarshal(raw, &p); err != nil {
			t.Fatalf("unmarshal %s: %v", w, err)
		}
		pkgs[p.GetImportPath()] = &p
	}
	return pkgs
}

func TestExtractHelloMetadata(t *testing.T) {
	pkgs := extractCorpus(t, helloDir, false)
	p, ok := pkgs["example.com/hello"]
	if !ok {
		t.Fatalf("missing package example.com/hello; got %v", keys(pkgs))
	}
	if p.GetSchemaVersion() != "1" {
		t.Errorf("schema_version = %q, want \"1\"", p.GetSchemaVersion())
	}
	if !strings.HasPrefix(p.GetGoVersion(), "go") {
		t.Errorf("go_version = %q, want go1.x", p.GetGoVersion())
	}
	if len(p.GetFiles()) != 1 || p.GetFiles()[0].GetPath() != "hello.go" {
		t.Fatalf("files = %v, want exactly [hello.go]", p.GetFiles())
	}
	if len(p.GetFiles()[0].GetSha256()) != 64 {
		t.Errorf("file sha256 = %q, want 64 hex chars", p.GetFiles()[0].GetSha256())
	}
}

func TestNoPackagesMatchedIsAnError(t *testing.T) {
	if _, err := run(t.TempDir(), []string{"./..."}, t.TempDir(), false); err == nil {
		t.Fatal("run() on an empty dir: want error, got nil")
	}
}

// TestBuildExcludedPackageDegrades pins spec §11 ("degrade, never die")
// against a real, legitimately-matched module whose only file is
// entirely excluded by a build constraint. go/packages reports this
// case very differently from an unresolvable pattern (see extract.go's
// comment in run()), but both can present as "nothing to extract" —
// this must NOT be treated the same as TestNoPackagesMatchedIsAnError's
// case: it should succeed with zero output, not fail.
func TestBuildExcludedPackageDegrades(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "go.mod"), []byte("module example.com/excluded\n\ngo 1.25\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	src := "//go:build never\n\npackage excluded\n\nfunc Never() {}\n"
	if err := os.WriteFile(filepath.Join(dir, "excluded.go"), []byte(src), 0o644); err != nil {
		t.Fatal(err)
	}

	written, err := run(dir, []string{"./..."}, t.TempDir(), false)
	if err != nil {
		t.Fatalf("run() on a build-excluded-only module: want nil error, got %v", err)
	}
	if len(written) != 0 {
		t.Errorf("run() on a build-excluded-only module: want no files written, got %v", written)
	}
}

func keys[V any](m map[string]*V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	return out
}

func findFunc(t *testing.T, p *gvirpb.Package, id string) *gvirpb.Function {
	t.Helper()
	for _, f := range p.GetFunctions() {
		if f.GetId() == id {
			return f
		}
	}
	ids := make([]string, 0, len(p.GetFunctions()))
	for _, f := range p.GetFunctions() {
		ids = append(ids, f.GetId())
	}
	t.Fatalf("function %q not found; have %v", id, ids)
	return nil
}

func TestExtractHelloFunctions(t *testing.T) {
	pkgs := extractCorpus(t, helloDir, false)
	p := pkgs["example.com/hello"]

	add := findFunc(t, p, "example.com/hello.Add")
	if len(add.GetParams()) != 2 || add.GetParams()[0].GetId() != 1 || add.GetParams()[1].GetId() != 2 {
		t.Errorf("Add params = %v, want ids 1,2", add.GetParams())
	}
	if len(add.GetBlocks()) == 0 {
		t.Error("Add has no basic blocks")
	}

	findFunc(t, p, "(*example.com/hello.Counter).Inc")
	findFunc(t, p, "example.com/hello.Spawn$1") // the goroutine closure

	// Every instruction register/operand id must resolve to a param,
	// aux value, or another instruction's register.
	for _, fn := range p.GetFunctions() {
		defined := map[uint32]bool{}
		for _, pa := range fn.GetParams() {
			defined[pa.GetId()] = true
		}
		for _, a := range fn.GetAux() {
			defined[a.GetId()] = true
		}
		for _, b := range fn.GetBlocks() {
			for _, ins := range b.GetInstrs() {
				if r := ins.GetRegister(); r != 0 {
					defined[r] = true
				}
			}
		}
		for _, b := range fn.GetBlocks() {
			for _, ins := range b.GetInstrs() {
				for _, op := range ins.GetOperands() {
					if op != 0 && !defined[op] {
						t.Errorf("%s: %s references undefined value id %d", fn.GetId(), ins.GetKind(), op)
					}
				}
			}
		}
	}
}

func TestSpawnHasGoAndSendInstructions(t *testing.T) {
	pkgs := extractCorpus(t, helloDir, false)
	p := pkgs["example.com/hello"]

	kinds := func(fn *gvirpb.Function) map[string]bool {
		out := map[string]bool{}
		for _, b := range fn.GetBlocks() {
			for _, ins := range b.GetInstrs() {
				out[ins.GetKind()] = true
			}
		}
		return out
	}
	if !kinds(findFunc(t, p, "example.com/hello.Spawn"))["Go"] {
		t.Error("Spawn: no Go instruction")
	}
	if !kinds(findFunc(t, p, "example.com/hello.Spawn$1"))["Send"] {
		t.Error("Spawn$1: no Send instruction")
	}
}
