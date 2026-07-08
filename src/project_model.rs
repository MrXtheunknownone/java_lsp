use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_project_model_has_no_modules() {
        assert_eq!(ProjectModel::default(), ProjectModel { modules: vec![] });
    }
}
