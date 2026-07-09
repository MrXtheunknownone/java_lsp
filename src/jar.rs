//! Reads `.class` entries out of a jar (a zip archive). Delegates the zip
//! container format and DEFLATE decompression to the `zip` crate rather than
//! reimplementing them — unlike classfile parsing, that's a non-trivial,
//! correctness-sensitive algorithm worth depending on rather than hand-rolling.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

#[derive(Debug)]
pub struct JarError(String);

impl std::fmt::Display for JarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Every non-directory `.class` entry in `jar_path`, as `(entry_name, bytes)`
/// pairs. Skips `module-info.class`, which describes the module itself rather
/// than a resolvable type.
pub fn class_entries(jar_path: &Path) -> Result<Vec<(String, Vec<u8>)>, JarError> {
    let file = File::open(jar_path).map_err(|err| JarError(format!("{jar_path:?}: {err}")))?;
    let mut archive =
        ZipArchive::new(file).map_err(|err| JarError(format!("{jar_path:?}: {err}")))?;

    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| JarError(format!("{jar_path:?}: {err}")))?;

        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if !name.ends_with(".class") || name == "module-info.class" {
            continue;
        }

        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|err| JarError(format!("{jar_path:?}: {name}: {err}")))?;
        entries.push((name, bytes));
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maven;
    use std::path::Path;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/classfiles")
            .join(name)
    }

    #[test]
    fn reads_class_entries_and_skips_directories_and_module_info() {
        let entries = class_entries(&fixture("sample.jar")).unwrap();

        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&"Simple.class"));
        assert!(names.contains(&"Greetable.class"));
        assert!(names.contains(&"sub/Impl.class"));
        assert!(!names.contains(&"module-info.class"));
        assert!(!names.iter().any(|name| name.ends_with('/')));
    }

    #[test]
    fn entry_bytes_round_trip_through_the_classfile_parser() {
        let entries = class_entries(&fixture("sample.jar")).unwrap();

        let (_, bytes) = entries
            .iter()
            .find(|(name, _)| name == "Simple.class")
            .unwrap();
        let class = crate::classfile::parse(bytes).unwrap();

        assert_eq!(class.this_class, "Simple");
    }

    #[test]
    fn returns_err_for_a_nonexistent_jar() {
        let result = class_entries(&fixture("does-not-exist.jar"));

        assert!(result.is_err());
    }

    #[test]
    fn reads_class_entries_from_a_real_third_party_dependency_jar() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/maven_sample");
        let model = maven::resolve_project_model(&root).expect("maven resolution should succeed");
        let app = model
            .modules
            .iter()
            .find(|m| m.root.ends_with("app"))
            .expect("app module present");
        let gson_jar = app
            .classpath
            .iter()
            .find(|entry| entry.file_name().is_some_and(|n| n == "gson-2.10.1.jar"))
            .expect("app's classpath should include gson");

        let entries = class_entries(gson_jar).unwrap();

        assert!(
            entries
                .iter()
                .any(|(name, _)| name == "com/google/gson/Gson.class")
        );
    }
}
