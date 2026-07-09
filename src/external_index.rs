//! Orchestrates Tier 3b: given a resolved dependency jar or the JDK, parses
//! every class, renders a synthetic stub `.java` source per class (see
//! `class_stub`), and feeds the result into the same `WorkspaceIndex` local
//! symbols already go through — the only module in Tier 3b that touches
//! `WorkspaceIndex` directly.

use crate::class_stub;
use crate::classfile::{self, ClassFile};
use crate::jar;
use crate::jdk_home::{self, ClassSource, JdkHome};
use crate::workspace_index::WorkspaceIndex;
use crate::workspace_scan;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// `$XDG_CACHE_HOME/java-lsp`, falling back to `$HOME/.cache/java-lsp`, then
/// a temp directory if neither is set — where extracted JDK classfiles and
/// rendered stub `.java` files are cached across server sessions.
pub fn default_cache_root() -> PathBuf {
    if let Some(xdg_cache_home) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg_cache_home).join("java-lsp");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".cache").join("java-lsp");
    }
    std::env::temp_dir().join("java-lsp-cache")
}

/// Indexes every class in a dependency jar. Silently does nothing if `entry`
/// isn't a readable jar — a slow or missing dependency must never block
/// indexing of the rest of the classpath.
pub fn index_classpath_entry(entry: &Path, cache_root: &Path, index: &Arc<Mutex<WorkspaceIndex>>) {
    let Ok(entries) = jar::class_entries(entry) else {
        return;
    };
    for (_, bytes) in entries {
        index_class_bytes(&bytes, cache_root, index);
    }
}

/// Indexes the JDK's `java.base` module (see `jdk_home::class_source`).
pub fn index_jdk(jdk: &JdkHome, cache_root: &Path, index: &Arc<Mutex<WorkspaceIndex>>) {
    let Ok(source) = jdk_home::class_source(jdk, cache_root) else {
        return;
    };
    match source {
        ClassSource::Jar(jar_path) => index_classpath_entry(&jar_path, cache_root, index),
        ClassSource::ExtractedDir(dir) => index_extracted_directory(&dir, cache_root, index),
    }
}

fn index_extracted_directory(dir: &Path, cache_root: &Path, index: &Arc<Mutex<WorkspaceIndex>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            index_extracted_directory(&path, cache_root, index);
        } else if is_class_file(&path)
            && let Ok(bytes) = std::fs::read(&path)
        {
            index_class_bytes(&bytes, cache_root, index);
        }
    }
}

fn is_class_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "class")
        && path
            .file_name()
            .is_some_and(|name| name != "module-info.class")
}

fn index_class_bytes(bytes: &[u8], cache_root: &Path, index: &Arc<Mutex<WorkspaceIndex>>) {
    let Ok(class) = classfile::parse(bytes) else {
        return;
    };
    let (_, simple_name) = class_stub::package_and_simple_name(&class.this_class);
    if !class_stub::is_valid_simple_name(&simple_name) {
        return;
    }
    let Ok(stub_path) = write_stub(&class, cache_root) else {
        return;
    };
    workspace_scan::index_source_file(&stub_path, index);
}

/// Renders and writes `class`'s synthetic stub, reusing an already-cached
/// file from a previous session without re-rendering it. Keyed by the full
/// binary name (not the simple name), so distinct nested classes that
/// happen to share a simple name (e.g. two different `Entry` inner classes)
/// never collide on the same file.
fn write_stub(class: &ClassFile, cache_root: &Path) -> std::io::Result<PathBuf> {
    let stub_path = cache_root
        .join("stubs")
        .join(format!("{}.java", class.this_class));
    if stub_path.is_file() {
        return Ok(stub_path);
    }
    if let Some(parent) = stub_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&stub_path, class_stub::render_stub(class))?;
    Ok(stub_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use crate::workspace_index::WorkspaceIndex;
    use std::path::Path;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/classfiles")
            .join(name)
    }

    fn stub_source(index: &Arc<Mutex<WorkspaceIndex>>, name: &str) -> String {
        let guard = index.lock().unwrap();
        let symbol = guard
            .lookup(name)
            .first()
            .unwrap_or_else(|| panic!("{name} was not indexed"))
            .clone();
        drop(guard);
        let path = crate::workspace_scan::uri_to_path(&symbol.uri).unwrap();
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn index_classpath_entry_indexes_every_class_in_a_real_jar() {
        let cache_root = TempDir::new("external-index-jar");
        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));

        index_classpath_entry(&fixture("sample.jar"), &cache_root.path, &index);

        let guard = index.lock().unwrap();
        assert!(!guard.lookup("Simple").is_empty());
        assert!(!guard.lookup("Greetable").is_empty());
        assert!(!guard.lookup("Impl").is_empty());
        drop(guard);

        assert!(stub_source(&index, "Impl").contains("implements Greetable"));
    }

    #[test]
    fn index_classpath_entry_produces_a_real_openable_file_with_real_content() {
        let cache_root = TempDir::new("external-index-jar-openable");
        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));

        index_classpath_entry(&fixture("sample.jar"), &cache_root.path, &index);

        let source = stub_source(&index, "Simple");
        assert!(source.contains("class Simple"));
        assert!(source.contains("int count"));
    }

    #[test]
    fn index_classpath_entry_does_nothing_for_a_nonexistent_jar() {
        let cache_root = TempDir::new("external-index-missing-jar");
        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));

        index_classpath_entry(&fixture("does-not-exist.jar"), &cache_root.path, &index);

        assert!(index.lock().unwrap().lookup("Simple").is_empty());
    }

    #[test]
    fn index_classpath_entry_reuses_a_cached_stub_on_a_second_call() {
        let cache_root = TempDir::new("external-index-cache-hit");
        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));

        index_classpath_entry(&fixture("sample.jar"), &cache_root.path, &index);
        let first_path =
            crate::workspace_scan::uri_to_path(&index.lock().unwrap().lookup("Simple")[0].uri)
                .unwrap();
        let modified_marker = "// tampered";
        let mut contents = std::fs::read_to_string(&first_path).unwrap();
        contents.push_str(modified_marker);
        std::fs::write(&first_path, &contents).unwrap();

        index_classpath_entry(&fixture("sample.jar"), &cache_root.path, &index);

        let contents_after = std::fs::read_to_string(&first_path).unwrap();
        assert!(
            contents_after.contains(modified_marker),
            "a cache hit must not re-render and overwrite the existing stub file"
        );
    }

    #[test]
    fn index_jdk_indexes_java_util_list_from_the_real_jdk() {
        let jdk =
            jdk_home::locate().expect("a real JDK should be locatable in this dev environment");
        let cache_root = TempDir::new("external-index-jdk");
        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));

        index_jdk(&jdk, &cache_root.path, &index);

        let guard = index.lock().unwrap();
        assert!(!guard.lookup("List").is_empty());
        drop(guard);
        assert!(stub_source(&index, "List").contains("interface List"));
    }
}
