use std::fmt;
use std::path::Path;

use crate::gvir;

pub const SCHEMA_VERSION: &str = "3";

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Decode(prost::DecodeError),
    SchemaVersion {
        found: String,
        expected: &'static str,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "reading .gvir: {e}"),
            LoadError::Decode(e) => write!(f, "decoding .gvir: {e}"),
            LoadError::SchemaVersion { found, expected } => write!(
                f,
                "unsupported .gvir schema version {found:?} (this goverify supports {expected:?}); re-extract with a matching extractor"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

pub fn load_package(path: &Path) -> Result<gvir::Package, LoadError> {
    use prost::Message;

    let bytes = std::fs::read(path).map_err(LoadError::Io)?;
    let pkg = gvir::Package::decode(bytes.as_slice()).map_err(LoadError::Decode)?;
    if pkg.schema_version != SCHEMA_VERSION {
        return Err(LoadError::SchemaVersion {
            found: pkg.schema_version,
            expected: SCHEMA_VERSION,
        });
    }
    Ok(pkg)
}

#[cfg(test)]
mod tests {
    use prost::Message;

    use super::*;

    fn write_pkg(dir: &tempfile::TempDir, pkg: &gvir::Package) -> std::path::PathBuf {
        let path = dir.path().join("p.gvir");
        std::fs::write(&path, pkg.encode_to_vec()).unwrap();
        path
    }

    #[test]
    fn round_trips_package() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = gvir::Package {
            schema_version: SCHEMA_VERSION.to_string(),
            import_path: "example.com/x".to_string(),
            ..Default::default()
        };
        let loaded = load_package(&write_pkg(&dir, &pkg)).expect("load_package");
        assert_eq!(loaded.import_path, "example.com/x");
    }

    #[test]
    fn rejects_wrong_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = gvir::Package {
            schema_version: "999".to_string(),
            ..Default::default()
        };
        match load_package(&write_pkg(&dir, &pkg)) {
            Err(LoadError::SchemaVersion { found, expected }) => {
                assert_eq!(found, "999");
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaVersion error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_garbage_bytes_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.gvir");
        std::fs::write(&path, [0xffu8; 64]).unwrap();
        assert!(matches!(load_package(&path), Err(LoadError::Decode(_))));
    }
}
