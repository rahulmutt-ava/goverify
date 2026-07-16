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
