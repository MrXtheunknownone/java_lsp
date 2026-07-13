use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct ModuleModel {
    pub root: PathBuf,
    pub source_dirs: Vec<PathBuf>,
    pub classpath: Vec<PathBuf>,
    pub java_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectModel {
    pub modules: Vec<ModuleModel>,
}

impl ProjectModel {
    /// The module whose `source_dirs` contains `file`, preferring the
    /// longest matching source directory when more than one module's source
    /// directories could contain it (e.g. nested modules).
    pub fn module_for_file(&self, file: &Path) -> Option<&ModuleModel> {
        self.modules
            .iter()
            .filter_map(|module| {
                module
                    .source_dirs
                    .iter()
                    .filter(|dir| file.starts_with(dir))
                    .map(|dir| dir.as_os_str().len())
                    .max()
                    .map(|best_len| (module, best_len))
            })
            .max_by_key(|(_, best_len)| *best_len)
            .map(|(module, _)| module)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(root: &str, source_dirs: &[&str]) -> ModuleModel {
        ModuleModel {
            root: PathBuf::from(root),
            source_dirs: source_dirs.iter().map(PathBuf::from).collect(),
            classpath: vec![],
            java_version: None,
        }
    }

    #[test]
    fn default_project_model_has_no_modules() {
        assert_eq!(ProjectModel::default(), ProjectModel { modules: vec![] });
    }

    #[test]
    fn module_for_file_finds_the_module_whose_source_dir_contains_the_file() {
        let model = ProjectModel {
            modules: vec![
                module("/repo/app", &["/repo/app/src/main/java"]),
                module("/repo/lib", &["/repo/lib/src/main/java"]),
            ],
        };

        let found = model
            .module_for_file(Path::new("/repo/lib/src/main/java/com/example/Lib.java"))
            .unwrap();

        assert_eq!(found.root, PathBuf::from("/repo/lib"));
    }

    #[test]
    fn module_for_file_returns_none_when_no_module_contains_the_file() {
        let model = ProjectModel {
            modules: vec![module("/repo/app", &["/repo/app/src/main/java"])],
        };

        assert!(
            model
                .module_for_file(Path::new("/elsewhere/Main.java"))
                .is_none()
        );
    }

    #[test]
    fn module_for_file_prefers_the_longest_matching_source_dir_when_modules_nest() {
        let model = ProjectModel {
            modules: vec![
                module("/repo", &["/repo/src/main/java"]),
                module(
                    "/repo/sub",
                    &["/repo/src/main/java/com/example/nested/src/main/java"],
                ),
            ],
        };

        let found = model
            .module_for_file(Path::new(
                "/repo/src/main/java/com/example/nested/src/main/java/Sub.java",
            ))
            .unwrap();

        assert_eq!(found.root, PathBuf::from("/repo/sub"));
    }
}
