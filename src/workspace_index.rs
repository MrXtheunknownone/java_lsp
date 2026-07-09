use crate::symbol::{self, SymbolInfo};
use lsp_types::Uri;
use std::collections::{BTreeMap, HashMap};

/// Generation tag for symbols indexed from disk during the initial workspace
/// scan, rather than from a client-versioned `didOpen`/`didChange` event. Any
/// real editor session (generation >= 1, assigned by `Server` on each
/// `didOpen`) supersedes it, in either arrival order.
pub const SCANNED_FROM_DISK: u64 = 0;

/// `(generation, version)`, compared lexicographically. `generation` is a
/// monotonically increasing counter bumped on every `didOpen` (see `Server`),
/// independent of the client-supplied LSP `version`, which editors are free to
/// restart from a low number on every fresh open. Gating on generation first
/// means a background reindex from a *closed* editor session — however late it
/// completes — can never overwrite a *later* session's content, which a
/// version-only comparison could not guarantee (a stale in-flight task from
/// before a fast close+reopen could carry a higher version number than the
/// reopened session's first edit).
type Applied = (u64, i32);

#[derive(Default)]
pub struct WorkspaceIndex {
    by_name: BTreeMap<String, Vec<SymbolInfo>>,
    by_uri: HashMap<Uri, Vec<SymbolInfo>>,
    applied: HashMap<Uri, Applied>,
}

impl WorkspaceIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the symbols indexed for `uri`. Returns `false` without changing
    /// anything if `(generation, version)` is older than what's already applied.
    pub fn update_file(
        &mut self,
        uri: Uri,
        generation: u64,
        version: i32,
        symbols: Vec<SymbolInfo>,
    ) -> bool {
        let key = (generation, version);
        if let Some(&applied) = self.applied.get(&uri)
            && key < applied
        {
            return false;
        }

        if let Some(old_symbols) = self.by_uri.remove(&uri) {
            for old in old_symbols {
                if let Some(bucket) = self.by_name.get_mut(&old.name) {
                    bucket.retain(|s| s.uri != uri);
                    if bucket.is_empty() {
                        self.by_name.remove(&old.name);
                    }
                }
            }
        }

        for symbol in &symbols {
            self.by_name
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.clone());
        }
        self.by_uri.insert(uri.clone(), symbols);
        self.applied.insert(uri, key);
        true
    }

    pub fn lookup(&self, name: &str) -> &[SymbolInfo] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// One entry per distinct matching name, preferring a real declaration
    /// over an `Import` of the same name (see `symbol::relevance_key`) so a
    /// class and an import of that class don't show up as two suggestions.
    ///
    /// `by_name` being a `BTreeMap` means matching names are a contiguous
    /// range starting at `prefix` — a `range` + `take_while` scan touches
    /// only the matching entries, rather than every distinct name in the
    /// index. That matters once external symbol resolution (Tier 3b) can
    /// add tens of thousands of JDK/library names: a full scan on every
    /// completion keystroke would violate the never-blocks rule.
    pub fn completions(&self, prefix: &str) -> Vec<&SymbolInfo> {
        let mut matches: Vec<&SymbolInfo> = self
            .by_name
            .range(prefix.to_string()..)
            .take_while(|(name, _)| name.starts_with(prefix))
            .flat_map(|(_, symbols)| symbols.iter())
            .collect();

        matches.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| symbol::relevance_key(a).cmp(&symbol::relevance_key(b)))
        });
        matches.dedup_by(|a, b| a.name == b.name);
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::SymbolKind;
    use lsp_types::{Position, Range};

    fn uri(value: &str) -> Uri {
        value.parse().unwrap()
    }

    fn symbol(name: &str, uri: Uri) -> SymbolInfo {
        let range = Range::new(Position::new(0, 0), Position::new(0, 1));
        SymbolInfo {
            name: name.to_string(),
            kind: SymbolKind::Class,
            uri,
            range,
            selection_range: range,
        }
    }

    #[test]
    fn lookup_returns_empty_for_an_unknown_name() {
        let index = WorkspaceIndex::new();

        assert!(index.lookup("Main").is_empty());
    }

    #[test]
    fn update_file_then_lookup_finds_the_symbol() {
        let mut index = WorkspaceIndex::new();
        let doc = uri("file:///Main.java");

        index.update_file(doc.clone(), 1, 1, vec![symbol("Main", doc)]);

        assert_eq!(index.lookup("Main").len(), 1);
    }

    #[test]
    fn update_file_with_a_lower_version_in_the_same_generation_is_rejected() {
        let mut index = WorkspaceIndex::new();
        let doc = uri("file:///Main.java");
        index.update_file(doc.clone(), 1, 5, vec![symbol("Main", doc.clone())]);

        let applied = index.update_file(doc.clone(), 1, 3, vec![symbol("Stale", doc)]);

        assert!(!applied);
        assert_eq!(index.lookup("Main").len(), 1);
        assert!(index.lookup("Stale").is_empty());
    }

    #[test]
    fn update_file_with_a_higher_version_replaces_old_symbols() {
        let mut index = WorkspaceIndex::new();
        let doc = uri("file:///Main.java");
        index.update_file(doc.clone(), 1, 1, vec![symbol("Old", doc.clone())]);

        let applied = index.update_file(doc.clone(), 1, 2, vec![symbol("New", doc)]);

        assert!(applied);
        assert!(index.lookup("Old").is_empty());
        assert_eq!(index.lookup("New").len(), 1);
    }

    #[test]
    fn a_disk_scanned_entry_is_superseded_by_a_real_generation_either_order() {
        let mut index = WorkspaceIndex::new();
        let doc = uri("file:///Main.java");

        index.update_file(doc.clone(), 1, 1, vec![symbol("FromOpen", doc.clone())]);
        let scan_applied = index.update_file(
            doc.clone(),
            SCANNED_FROM_DISK,
            0,
            vec![symbol("FromDisk", doc.clone())],
        );

        assert!(!scan_applied);
        assert_eq!(index.lookup("FromOpen").len(), 1);
        assert!(index.lookup("FromDisk").is_empty());

        let mut index_other_order = WorkspaceIndex::new();
        index_other_order.update_file(
            doc.clone(),
            SCANNED_FROM_DISK,
            0,
            vec![symbol("FromDisk", doc.clone())],
        );
        let open_applied =
            index_other_order.update_file(doc.clone(), 1, 1, vec![symbol("FromOpen", doc)]);

        assert!(open_applied);
        assert!(index_other_order.lookup("FromDisk").is_empty());
        assert_eq!(index_other_order.lookup("FromOpen").len(), 1);
    }

    #[test]
    fn a_later_generation_always_wins_even_with_a_lower_version_number() {
        // Simulates: edit up to version 10, close, reopen (new generation) at
        // version 1, and a stale in-flight reindex from before the close
        // finally completing afterwards — it must not clobber the reopen.
        let mut index = WorkspaceIndex::new();
        let doc = uri("file:///Main.java");

        index.update_file(doc.clone(), 1, 1, vec![symbol("New", doc.clone())]);
        let stale_applied =
            index.update_file(doc.clone(), 0, 10, vec![symbol("StaleOld", doc.clone())]);

        assert!(!stale_applied);
        assert_eq!(index.lookup("New").len(), 1);
        assert!(index.lookup("StaleOld").is_empty());
    }

    #[test]
    fn completions_filters_by_prefix_across_files() {
        let mut index = WorkspaceIndex::new();
        index.update_file(
            uri("file:///Main.java"),
            1,
            1,
            vec![symbol("Greeter", uri("file:///Main.java"))],
        );
        index.update_file(
            uri("file:///Other.java"),
            1,
            1,
            vec![symbol("Greeting", uri("file:///Other.java"))],
        );

        let mut names: Vec<&str> = index
            .completions("Gree")
            .into_iter()
            .map(|s| s.name.as_str())
            .collect();
        names.sort_unstable();

        assert_eq!(names, vec!["Greeter", "Greeting"]);
        assert!(index.completions("Zzz").is_empty());
    }

    #[test]
    fn completions_deduplicates_a_class_and_an_import_of_the_same_name() {
        let mut index = WorkspaceIndex::new();
        let greeter_uri = uri("file:///Greeter.java");
        let main_uri = uri("file:///Main.java");
        index.update_file(
            greeter_uri.clone(),
            1,
            1,
            vec![symbol("Greeter", greeter_uri)],
        );
        index.update_file(
            main_uri.clone(),
            1,
            1,
            vec![SymbolInfo {
                name: "Greeter".to_string(),
                kind: SymbolKind::Import,
                uri: main_uri,
                range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                selection_range: Range::new(Position::new(0, 0), Position::new(0, 1)),
            }],
        );

        let matches = index.completions("Gree");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].kind, SymbolKind::Class);
    }

    #[test]
    fn completions_finds_only_matching_names_among_many_decoy_entries() {
        let mut index = WorkspaceIndex::new();
        for i in 0..20_000 {
            let name = format!("Decoy{i}");
            let entry_uri = uri(&format!("file:///decoy/{i}.java"));
            index.update_file(entry_uri.clone(), 1, 1, vec![symbol(&name, entry_uri)]);
        }
        let target_uri = uri("file:///Target.java");
        index.update_file(
            target_uri.clone(),
            1,
            1,
            vec![symbol("TargetType", target_uri)],
        );

        let names: Vec<&str> = index
            .completions("Target")
            .into_iter()
            .map(|s| s.name.as_str())
            .collect();

        assert_eq!(names, vec!["TargetType"]);
    }

    #[test]
    fn completions_stays_fast_with_tens_of_thousands_of_distinct_names() {
        let mut index = WorkspaceIndex::new();
        for i in 0..50_000 {
            // Fixed-width so no name is a prefix of another distinct name.
            let name = format!("java.util.Decoy{i:05}");
            let entry_uri = uri(&format!("file:///decoy/{i}.java"));
            index.update_file(entry_uri.clone(), 1, 1, vec![symbol(&name, entry_uri)]);
        }

        let start = std::time::Instant::now();
        let matches = index.completions("java.util.Decoy01234");
        let elapsed = start.elapsed();

        assert_eq!(matches.len(), 1);
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "completions took {elapsed:?}, expected a range query rather than a full scan"
        );
    }
}
