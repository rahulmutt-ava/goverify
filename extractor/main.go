// The goverify extractor sidecar: emits one canonicalized .gvir
// protobuf per Go package. Built and driven by goverify-extract (Rust);
// its CLI is a private contract, not a user surface.
package main

import (
	"flag"
	"fmt"
	"os"
)

func main() {
	out := flag.String("out", "", "directory to write .gvir files (required)")
	deps := flag.Bool("deps", true, "also extract all dependency packages")
	manifestMode := flag.Bool("manifest", false, "print the import-closure manifest (pkg/dep/file lines) instead of extracting")
	flag.Parse()
	if *manifestMode {
		if flag.NArg() == 0 {
			fmt.Fprintln(os.Stderr, "usage: extractor -manifest PATTERN...")
			os.Exit(2)
		}
		if err := manifest("", flag.Args(), os.Stdout); err != nil {
			fmt.Fprintln(os.Stderr, "extractor:", err)
			os.Exit(1)
		}
		return
	}
	if *out == "" || flag.NArg() == 0 {
		fmt.Fprintln(os.Stderr, "usage: extractor -out DIR [-deps=false] PATTERN...")
		os.Exit(2)
	}
	written, err := run("", flag.Args(), *out, *deps)
	if err != nil {
		fmt.Fprintln(os.Stderr, "extractor:", err)
		os.Exit(1)
	}
	for _, p := range written {
		fmt.Println(p)
	}
}
