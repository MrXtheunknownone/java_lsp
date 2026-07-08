use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildTool {
    Gradle,
    Maven,
}

pub fn detect(root: &Path) -> Option<BuildTool> {
    let gradle_markers = [
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
    ];
    if gradle_markers
        .iter()
        .any(|marker| root.join(marker).is_file())
    {
        return Some(BuildTool::Gradle);
    }

    if root.join("pom.xml").is_file() {
        return Some(BuildTool::Maven);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn detects_maven_from_pom_xml() {
        let temp = TempDir::new("build-tool");
        std::fs::write(temp.path.join("pom.xml"), "").unwrap();

        assert_eq!(detect(&temp.path), Some(BuildTool::Maven));
    }

    #[test]
    fn detects_gradle_from_build_gradle() {
        let temp = TempDir::new("build-tool");
        std::fs::write(temp.path.join("build.gradle"), "").unwrap();

        assert_eq!(detect(&temp.path), Some(BuildTool::Gradle));
    }

    #[test]
    fn detects_gradle_from_build_gradle_kts() {
        let temp = TempDir::new("build-tool");
        std::fs::write(temp.path.join("build.gradle.kts"), "").unwrap();

        assert_eq!(detect(&temp.path), Some(BuildTool::Gradle));
    }

    #[test]
    fn detects_gradle_from_settings_gradle_alone() {
        let temp = TempDir::new("build-tool");
        std::fs::write(temp.path.join("settings.gradle"), "").unwrap();

        assert_eq!(detect(&temp.path), Some(BuildTool::Gradle));
    }

    #[test]
    fn prefers_gradle_when_both_markers_are_present() {
        let temp = TempDir::new("build-tool");
        std::fs::write(temp.path.join("pom.xml"), "").unwrap();
        std::fs::write(temp.path.join("build.gradle"), "").unwrap();

        assert_eq!(detect(&temp.path), Some(BuildTool::Gradle));
    }

    #[test]
    fn detects_nothing_when_no_marker_is_present() {
        let temp = TempDir::new("build-tool");

        assert_eq!(detect(&temp.path), None);
    }
}
