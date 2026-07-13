//! Tier 3a fallback for workspaces with no detected build tool
//! ([`crate::build_tool::detect`] returns `None`). Per CLAUDE.md, this path
//! only supports classpath-free, JDK-only code — it cannot resolve external
//! dependencies, so `classpath` is always empty. Its real purpose is to
//! still produce a [`ProjectModel`], so `Server::index_external_symbols`
//! (JDK `java.base` indexing) runs even for a build-tool-less workspace,
//! which today it silently doesn't.

use crate::jdk_home;
use crate::maven;
use crate::project_model::{ModuleModel, ProjectModel};
use std::path::Path;

/// Infallible: everything here is a directory check plus the already-cheap
/// `jdk_home::locate()` lookup — there is no failure mode worth a `Result`.
pub fn resolve_project_model(root: &Path) -> ProjectModel {
    let standard_dirs = maven::standard_source_dirs(root);
    let source_dirs = if standard_dirs.is_empty() {
        vec![root.to_path_buf()]
    } else {
        standard_dirs
    };
    let java_version = jdk_home::locate()
        .and_then(|jdk| jdk.major_version)
        .map(|version| version.to_string());

    ProjectModel {
        modules: vec![ModuleModel {
            root: root.to_path_buf(),
            source_dirs,
            classpath: vec![],
            java_version,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn uses_the_standard_maven_style_source_dirs_when_present() {
        let temp = TempDir::new("javac-fallback");
        std::fs::create_dir_all(temp.path.join("src/main/java")).unwrap();

        let model = resolve_project_model(&temp.path);

        assert_eq!(model.modules.len(), 1);
        assert_eq!(
            model.modules[0].source_dirs,
            vec![temp.path.join("src/main/java")]
        );
    }

    #[test]
    fn falls_back_to_the_root_itself_when_no_standard_source_dirs_exist() {
        let temp = TempDir::new("javac-fallback");
        std::fs::write(temp.path.join("Main.java"), "class Main {}").unwrap();

        let model = resolve_project_model(&temp.path);

        assert_eq!(model.modules[0].source_dirs, vec![temp.path.clone()]);
    }

    #[test]
    fn classpath_is_always_empty() {
        let temp = TempDir::new("javac-fallback");

        let model = resolve_project_model(&temp.path);

        assert!(model.modules[0].classpath.is_empty());
    }

    #[test]
    fn populates_java_version_from_the_located_jdk() {
        let temp = TempDir::new("javac-fallback");

        let model = resolve_project_model(&temp.path);

        assert!(model.modules[0].java_version.is_some());
    }

    #[test]
    fn module_root_is_the_workspace_root() {
        let temp = TempDir::new("javac-fallback");

        let model = resolve_project_model(&temp.path);

        assert_eq!(model.modules[0].root, temp.path);
    }
}
