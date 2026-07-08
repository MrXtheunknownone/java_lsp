use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// Serializes tests that invoke `gradle` against `tests/fixtures/gradle_sample` —
/// two concurrent Gradle invocations against the same project directory can fail
/// outright (shared `build/`/`.gradle` state), not just run slowly.
pub(crate) static GRADLE_SAMPLE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct TempDir {
    pub(crate) path: PathBuf,
}

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("java-lsp-{label}-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_a_directory_that_drop_removes() {
        let path = {
            let temp = TempDir::new("test-support");
            assert!(temp.path.is_dir());
            temp.path.clone()
        };

        assert!(!path.exists());
    }
}
