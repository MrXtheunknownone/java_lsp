//! Orchestrates Tier 3b: given a resolved dependency jar or the JDK, parses
//! every class, renders a synthetic stub `.java` source per class (see
//! `class_stub`), and feeds the result into the same `WorkspaceIndex` local
//! symbols already go through — the only module in Tier 3b that touches
//! `WorkspaceIndex` directly.

use crate::class_stub;
use crate::classfile::{self, ClassFile};
use crate::jar;
use crate::jdk_home::{self, ClassSource, JdkHome};
use crate::symbol::SymbolInfo;
use crate::workspace_index::WorkspaceIndex;
use crate::workspace_scan;
use std::collections::HashMap;
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
/// indexing of the rest of the classpath. Jar content is immutable for the
/// life of a session, so a previously cached stub is always reused.
pub fn index_classpath_entry(entry: &Path, cache_root: &Path, index: &Arc<Mutex<WorkspaceIndex>>) {
    let Ok(entries) = jar::class_entries(entry) else {
        return;
    };
    for (_, bytes) in entries {
        index_class_bytes(&bytes, cache_root, index, false);
    }
}

/// Indexes the JDK's `java.base` module (see `jdk_home::class_source`).
pub fn index_jdk(jdk: &JdkHome, cache_root: &Path, index: &Arc<Mutex<WorkspaceIndex>>) {
    let Ok(source) = jdk_home::class_source(jdk, cache_root) else {
        return;
    };
    match source {
        ClassSource::Jar(jar_path) => index_classpath_entry(&jar_path, cache_root, index),
        ClassSource::ExtractedDir(dir) => index_class_directory(&dir, cache_root, index, false),
    }
}

/// Indexes every classfile a `javac` compile just produced (see
/// `javac_compile::compile`). Unlike jar/JDK content, this comes from
/// source the user is actively editing and re-saving — a previously cached
/// stub is always overwritten, never reused, so edits are reflected.
pub fn index_javac_output(dir: &Path, cache_root: &Path, index: &Arc<Mutex<WorkspaceIndex>>) {
    index_class_directory(dir, cache_root, index, true);
}

fn index_class_directory(
    dir: &Path,
    cache_root: &Path,
    index: &Arc<Mutex<WorkspaceIndex>>,
    overwrite: bool,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            index_class_directory(&path, cache_root, index, overwrite);
        } else if is_class_file(&path)
            && let Ok(bytes) = std::fs::read(&path)
        {
            index_class_bytes(&bytes, cache_root, index, overwrite);
        }
    }
}

fn is_class_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "class")
        && path
            .file_name()
            .is_some_and(|name| name != "module-info.class")
}

/// `jdk.internal.*` (module-encapsulated, uncompilable against without
/// special flags since JDK 9) and `sun.*` (documented as unsupported,
/// non-public API since Java's earliest days) can never be referenced by
/// ordinary application code — indexing them only adds noise to same-name
/// lookups for common accessor names.
fn is_jdk_internal_class(this_class: &str) -> bool {
    this_class.starts_with("jdk/internal/") || this_class.starts_with("sun/")
}

fn index_class_bytes(
    bytes: &[u8],
    cache_root: &Path,
    index: &Arc<Mutex<WorkspaceIndex>>,
    overwrite: bool,
) {
    let Ok(class) = classfile::parse(bytes) else {
        return;
    };
    if is_jdk_internal_class(&class.this_class) {
        return;
    }
    let (_, simple_name) = class_stub::package_and_simple_name(&class.this_class);
    if !class_stub::is_valid_simple_name(&simple_name) {
        return;
    }
    let Ok(stub_path) = write_stub(&class, cache_root, overwrite) else {
        return;
    };
    workspace_scan::index_source_file(&stub_path, index);
}

/// Renders and writes `class`'s synthetic stub. Keyed by the full binary
/// name (not the simple name), so distinct nested classes that happen to
/// share a simple name (e.g. two different `Entry` inner classes) never
/// collide on the same file. When `overwrite` is false and a stub already
/// exists (the immutable jar/JDK case), it's reused as-is rather than
/// re-rendered.
pub(crate) fn write_stub(
    class: &ClassFile,
    cache_root: &Path,
    overwrite: bool,
) -> std::io::Result<PathBuf> {
    let stub_path = cache_root
        .join("stubs")
        .join(format!("{}.java", class.this_class));
    if stub_path.is_file() && !overwrite {
        return Ok(stub_path);
    }
    if let Some(parent) = stub_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&stub_path, class_stub::render_stub(class))?;
    Ok(stub_path)
}

/// Re-indexes a single already-parsed class's stub, applying `overrides` to
/// its extracted symbols (see `workspace_scan::index_source_file_with_overrides`)
/// — used by the Lombok pipeline to exclude hand-written duplicates and
/// redirect generated accessors to their backing fields. Always overwrites
/// the cached stub, matching `index_javac_output`'s cache-staleness rule for
/// actively-edited source.
pub fn reindex_class_file_with_overrides(
    class: &ClassFile,
    cache_root: &Path,
    index: &Arc<Mutex<WorkspaceIndex>>,
    overrides: &HashMap<String, Option<SymbolInfo>>,
) {
    let (_, simple_name) = class_stub::package_and_simple_name(&class.this_class);
    if !class_stub::is_valid_simple_name(&simple_name) {
        return;
    }
    let Ok(stub_path) = write_stub(class, cache_root, true) else {
        return;
    };
    workspace_scan::index_source_file_with_overrides(&stub_path, index, overrides);
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
    fn skips_jdk_internal_and_sun_packages() {
        let cache_root = TempDir::new("external-index-jdk-internal-skip");
        let class_dir = TempDir::new("external-index-jdk-internal-skip-classes");
        std::fs::create_dir_all(class_dir.path.join("jdk/internal")).unwrap();
        std::fs::write(
            class_dir.path.join("jdk/internal/Foo.class"),
            minimal_classfile_bytes("jdk/internal/Foo"),
        )
        .unwrap();
        std::fs::create_dir_all(class_dir.path.join("sun/misc")).unwrap();
        std::fs::write(
            class_dir.path.join("sun/misc/Bar.class"),
            minimal_classfile_bytes("sun/misc/Bar"),
        )
        .unwrap();
        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));

        index_javac_output(&class_dir.path, &cache_root.path, &index);

        let guard = index.lock().unwrap();
        assert!(guard.lookup("Foo").is_empty());
        assert!(guard.lookup("Bar").is_empty());
    }

    fn minimal_classfile_bytes(class_name: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]); // minor_version
        bytes.extend_from_slice(&[0, 65]); // major_version

        // Constant pool: #1 Utf8 class_name, #2 Class -> #1,
        // #3 Utf8 "java/lang/Object", #4 Class -> #3.
        bytes.extend_from_slice(&[0, 5]); // constant_pool_count = 5 (4 real entries)
        push_utf8(&mut bytes, class_name);
        push_class(&mut bytes, 1);
        push_utf8(&mut bytes, "java/lang/Object");
        push_class(&mut bytes, 3);

        bytes.extend_from_slice(&[0, 0x21]); // access_flags: ACC_PUBLIC | ACC_SUPER
        bytes.extend_from_slice(&[0, 2]); // this_class -> #2
        bytes.extend_from_slice(&[0, 4]); // super_class -> #4
        bytes.extend_from_slice(&[0, 0]); // interfaces_count
        bytes.extend_from_slice(&[0, 0]); // fields_count
        bytes.extend_from_slice(&[0, 0]); // methods_count
        bytes.extend_from_slice(&[0, 0]); // attributes_count
        bytes
    }

    fn push_utf8(bytes: &mut Vec<u8>, value: &str) {
        bytes.push(1); // tag: Utf8
        bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }

    fn push_class(bytes: &mut Vec<u8>, name_index: u16) {
        bytes.push(7); // tag: Class
        bytes.extend_from_slice(&name_index.to_be_bytes());
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
    fn index_javac_output_always_overwrites_a_stale_cached_stub() {
        let cache_root = TempDir::new("external-index-javac-output");
        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));
        let class_dir = TempDir::new("external-index-javac-output-classes");
        std::fs::copy(fixture("Simple.class"), class_dir.path.join("Simple.class")).unwrap();

        index_javac_output(&class_dir.path, &cache_root.path, &index);
        let first_path =
            crate::workspace_scan::uri_to_path(&index.lock().unwrap().lookup("Simple")[0].uri)
                .unwrap();
        let modified_marker = "// tampered";
        let mut contents = std::fs::read_to_string(&first_path).unwrap();
        contents.push_str(modified_marker);
        std::fs::write(&first_path, &contents).unwrap();

        index_javac_output(&class_dir.path, &cache_root.path, &index);

        let contents_after = std::fs::read_to_string(&first_path).unwrap();
        assert!(
            !contents_after.contains(modified_marker),
            "javac output must always be re-rendered, never reused from a stale cache"
        );
    }

    #[test]
    fn reindex_class_file_with_overrides_excludes_and_redirects_by_name() {
        let cache_root = TempDir::new("external-index-overrides");
        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));
        let bytes = std::fs::read(fixture("Simple.class")).unwrap();
        let class = classfile::parse(&bytes).unwrap();

        let redirect_uri: lsp_types::Uri = "file:///Elsewhere.java".parse().unwrap();
        let redirect_range = lsp_types::Range::new(
            lsp_types::Position::new(2, 0),
            lsp_types::Position::new(2, 1),
        );
        let mut overrides: HashMap<String, Option<SymbolInfo>> = HashMap::new();
        overrides.insert("getCount".to_string(), None);
        overrides.insert(
            "setName".to_string(),
            Some(SymbolInfo {
                name: "setName".to_string(),
                kind: crate::symbol::SymbolKind::Method,
                uri: redirect_uri.clone(),
                range: redirect_range,
                selection_range: redirect_range,
                owner: None,
            }),
        );

        reindex_class_file_with_overrides(&class, &cache_root.path, &index, &overrides);

        let guard = index.lock().unwrap();
        assert!(guard.lookup("getCount").is_empty());
        let redirected = guard.lookup("setName");
        assert_eq!(redirected.len(), 1);
        assert_eq!(redirected[0].uri, redirect_uri);
        assert_eq!(redirected[0].range, redirect_range);
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
