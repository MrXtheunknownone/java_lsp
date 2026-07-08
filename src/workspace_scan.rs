use crate::symbol::extract_symbols;
use crate::syntax::SyntaxTree;
use crate::workspace_index::{SCANNED_FROM_DISK, WorkspaceIndex};
use lsp_types::Uri;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub fn find_java_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_java_files(root, &mut files);
    files
}

fn collect_java_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let is_hidden = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'));
            if !is_hidden {
                collect_java_files(&path, files);
            }
        } else if path.extension().is_some_and(|ext| ext == "java") {
            files.push(path);
        }
    }
}

pub fn index_workspace(root: &Path, index: &Arc<Mutex<WorkspaceIndex>>) {
    for path in find_java_files(root) {
        let Some(uri) = path_to_uri(&path) else {
            continue;
        };
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };

        let tree = SyntaxTree::parse(&source);
        let symbols = extract_symbols(&uri, tree.source(), tree.tree());

        let mut guard = index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.update_file(uri, SCANNED_FROM_DISK, 0, symbols);
    }
}

fn path_to_uri(path: &Path) -> Option<Uri> {
    // Intentionally not canonicalized: canonicalizing (resolving symlinks) here
    // while didOpen/didChange use the client's raw Uri would index the same file
    // under two different Uri keys whenever a symlink is involved.
    let normalized = normalize_path_for_uri(&path.display().to_string());
    format!("file://{}", percent_encode_path(&normalized))
        .parse()
        .ok()
}

fn normalize_path_for_uri(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    normalized
}

pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    if !uri
        .scheme()
        .is_some_and(|scheme| scheme.eq_lowercase("file"))
    {
        return None;
    }
    let decoded = percent_decode_path(uri.path().as_str());
    Some(PathBuf::from(strip_windows_drive_prefix(&decoded)))
}

fn strip_windows_drive_prefix(path: &str) -> &str {
    let bytes = path.as_bytes();
    let is_drive_prefixed =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    if is_drive_prefixed { &path[1..] } else { path }
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn percent_decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(value) = hex {
                decoded.push(value);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn find_java_files_finds_nested_java_files_and_skips_hidden_dirs_and_other_extensions() {
        let temp = TempDir::new("workspace-scan");
        std::fs::write(temp.path.join("Main.java"), "class Main {}").unwrap();
        std::fs::create_dir_all(temp.path.join("util")).unwrap();
        std::fs::write(temp.path.join("util/Greeter.java"), "class Greeter {}").unwrap();
        std::fs::create_dir_all(temp.path.join(".git")).unwrap();
        std::fs::write(temp.path.join(".git/ignored.java"), "class Ignored {}").unwrap();
        std::fs::write(temp.path.join("NotJava.txt"), "not java").unwrap();

        let mut files = find_java_files(&temp.path);
        files.sort();

        let mut expected = vec![
            temp.path.join("Main.java"),
            temp.path.join("util/Greeter.java"),
        ];
        expected.sort();
        assert_eq!(files, expected);
    }

    #[test]
    fn uri_to_path_extracts_the_filesystem_path_from_a_file_uri() {
        let uri: Uri = "file:///home/tim/testbed/Main.java".parse().unwrap();

        let path = uri_to_path(&uri).unwrap();

        assert_eq!(path, PathBuf::from("/home/tim/testbed/Main.java"));
    }

    #[test]
    fn uri_to_path_returns_none_for_a_non_file_scheme() {
        let uri: Uri = "https://example.com/Main.java".parse().unwrap();

        assert!(uri_to_path(&uri).is_none());
    }

    #[test]
    fn uri_to_path_decodes_percent_encoded_spaces() {
        let uri: Uri = "file:///home/tim/My%20Project/Main.java".parse().unwrap();

        let path = uri_to_path(&uri).unwrap();

        assert_eq!(path, PathBuf::from("/home/tim/My Project/Main.java"));
    }

    #[test]
    fn uri_to_path_strips_the_leading_slash_before_a_windows_drive_letter() {
        let uri: Uri = "file:///C:/Users/me/Main.java".parse().unwrap();

        let path = uri_to_path(&uri).unwrap();

        assert_eq!(path, PathBuf::from("C:/Users/me/Main.java"));
    }

    #[test]
    fn normalize_path_for_uri_converts_backslashes_and_adds_a_leading_slash() {
        assert_eq!(
            normalize_path_for_uri("C:\\Users\\me\\Main.java"),
            "/C:/Users/me/Main.java"
        );
    }

    #[test]
    fn normalize_path_for_uri_leaves_a_unix_path_unchanged() {
        assert_eq!(
            normalize_path_for_uri("/home/tim/Main.java"),
            "/home/tim/Main.java"
        );
    }

    #[test]
    fn path_to_uri_and_uri_to_path_round_trip_a_windows_style_path() {
        let uri: Uri = format!(
            "file://{}",
            percent_encode_path(&normalize_path_for_uri("C:\\Users\\me\\Main.java"))
        )
        .parse()
        .unwrap();

        let path = uri_to_path(&uri).unwrap();

        assert_eq!(path, PathBuf::from("C:/Users/me/Main.java"));
    }

    #[cfg(unix)]
    #[test]
    fn path_to_uri_does_not_resolve_symlinks() {
        let temp = TempDir::new("workspace-scan-symlink");
        let real_dir = temp.path.join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join("Main.java"), "class Main {}").unwrap();
        let link = temp.path.join("link");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let via_link = path_to_uri(&link.join("Main.java")).unwrap();
        let via_link_again = path_to_uri(&link.join("Main.java")).unwrap();

        assert_eq!(via_link, via_link_again);
        assert!(via_link.as_str().contains("/link/"));
    }

    #[test]
    fn index_workspace_indexes_a_file_under_a_path_containing_a_space() {
        let temp = TempDir::new("workspace-scan");
        let project_dir = temp.path.join("My Project");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("Main.java"), "class Main {}").unwrap();

        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));
        index_workspace(&temp.path, &index);

        let guard = index.lock().unwrap();
        assert_eq!(guard.lookup("Main").len(), 1);
    }

    #[test]
    fn index_workspace_indexes_every_discovered_file() {
        let temp = TempDir::new("workspace-scan");
        std::fs::write(temp.path.join("Main.java"), "class Main {}").unwrap();
        std::fs::write(temp.path.join("Greeter.java"), "class Greeter {}").unwrap();

        let index = Arc::new(Mutex::new(WorkspaceIndex::new()));
        index_workspace(&temp.path, &index);

        let guard = index.lock().unwrap();
        assert_eq!(guard.lookup("Main").len(), 1);
        assert_eq!(guard.lookup("Greeter").len(), 1);
    }
}
