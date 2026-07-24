//! Integration-test helpers: extract a corpus module through the real
//! sidecar and load it. Not part of the analyzer API.

use std::path::{Path, PathBuf};

use goverify_extract::Sidecar;

use crate::program::Program;

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Extract testdata/corpus/<module> (whole DAG) into a kept temp dir and
/// load it. Panics on failure — test-only code.
pub fn load_corpus(module: &str) -> Program {
    let root = repo_root();
    let sc = Sidecar::build(&root.join("extractor"), &root.join("target/extractor-bin"))
        .expect("Sidecar::build");
    let dir = tempfile::tempdir().expect("tempdir").keep();
    sc.extract(&root.join("testdata/corpus").join(module), &["./..."], &dir)
        .expect("extract");
    Program::load_dir(&dir).expect("load_dir")
}

/// Byte-exact golden comparison. UPDATE_GOLDENS=1 rewrites the file;
/// always review the diff by hand before committing.
pub fn check_golden(name: &str, got: &str) {
    let path = repo_root().join("testdata/goldens").join(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing golden {name} ({e}); run with UPDATE_GOLDENS=1"));
    assert_eq!(
        want, got,
        "golden {name} drifted; review + UPDATE_GOLDENS=1 if intended"
    );
}

/// Machine-checked corpus expectations (phase-4 spec §6): `// want: tag`
/// (comma-separated for several on one line) attached to the line it
/// annotates. Returns (file name, 1-based line, tag) sorted.
pub fn wants(module: &str) -> Vec<(String, u32, String)> {
    wants_in(&repo_root().join("testdata/corpus").join(module))
}

/// The parser behind `wants`, taking an explicit directory so it's testable
/// without depending on a real corpus module.
pub fn wants_in(dir: &Path) -> Vec<(String, u32, String)> {
    let mut out = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("corpus dir {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "go"))
        .collect();
    files.sort();
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        for (i, line) in text.lines().enumerate() {
            // Trailing-comment position only (wave-2 follow-up, twice
            // bitten by prose): the marker must be the line's LAST `//`
            // comment, there must be real code before it, and every tag
            // must be a bare [a-z0-9-]+ token. Anything else is prose.
            let Some(idx) = line.rfind("//") else {
                continue;
            };
            let (code, comment) = line.split_at(idx);
            let Some(rest) = comment.strip_prefix("//") else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix("want:") else {
                continue;
            };
            if code.trim().is_empty() || code.trim_start().starts_with("//") {
                continue; // whole-line or comment-only prefix: prose, not a pin
            }
            let tags: Vec<&str> = rest.split(',').map(str::trim).collect();
            let valid = |t: &str| {
                !t.is_empty()
                    && t.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            };
            if !tags.iter().all(|t| valid(t)) {
                continue; // prose after "want:" — not a tag list
            }
            for tag in tags {
                out.push((name.clone(), (i + 1) as u32, tag.to_string()));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_parses_tags_lines_and_multi() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("a.go"),
            "package a\nfunc f() {} // want: nil-deref\n_ = x // want: bounds, div-zero\n",
        )
        .unwrap();
        assert_eq!(
            wants_in(d.path()),
            vec![
                ("a.go".into(), 2, "nil-deref".into()),
                ("a.go".into(), 3, "bounds".into()),
                ("a.go".into(), 3, "div-zero".into()),
            ]
        );
    }

    #[test]
    fn wants_ignores_prose_and_whole_line_comments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.go"),
            concat!(
                "package a\n",
                "// This pin exists because the // want: parser used to\n", // prose: whole-line comment
                "// match `// want: overflow` anywhere in a line.\n", // prose: marker mid-comment
                "func F(x int) int {\n",
                "\treturn x + x // want: overflow\n", // real pin
                "}\n",
                "// want: nil-deref\n", // whole-line marker: NOT a pin
                "func G() { _ = 1 } // want: not a valid tag list\n", // invalid tags: NOT a pin
            ),
        )
        .unwrap();
        let got = wants_in(dir.path());
        assert_eq!(
            got,
            vec![("a.go".to_string(), 5, "overflow".to_string())],
            "wants_in(): only the trailing-comment marker with valid tags parses"
        );
    }
}
