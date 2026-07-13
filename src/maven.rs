use crate::project_model::{ModuleModel, ProjectModel};
use std::path::{Path, PathBuf};
use std::process::Command;

const CLASSPATH_FILE: &str = "target/java-lsp-classpath.txt";

pub fn resolve_project_model(root: &Path) -> Result<ProjectModel, String> {
    let module_dirs = discover_modules(root);

    run_maven(
        root,
        &[
            "-q",
            "-B",
            "package",
            "dependency:build-classpath",
            &format!("-Dmdep.outputFile={CLASSPATH_FILE}"),
            "-DskipTests",
        ],
    )?;

    let modules = module_dirs
        .into_iter()
        .map(|module_dir| {
            let classpath = read_classpath(&module_dir.join(CLASSPATH_FILE));
            let source_dirs = standard_source_dirs(&module_dir);
            // Resolved per module (not once at the root) so a child pom.xml that
            // overrides maven.compiler.* is honored; a module without its own
            // override still resolves correctly since Maven evaluates it with
            // the parent's inherited property value.
            let java_version = resolve_java_version(&module_dir);
            ModuleModel {
                root: module_dir,
                source_dirs,
                classpath,
                java_version,
            }
        })
        .collect();

    Ok(ProjectModel { modules })
}

fn discover_modules(root: &Path) -> Vec<PathBuf> {
    let mut discovered = Vec::new();
    collect_modules(root, &mut discovered);
    discovered
}

fn collect_modules(dir: &Path, discovered: &mut Vec<PathBuf>) {
    let Ok(pom_text) = std::fs::read_to_string(dir.join("pom.xml")) else {
        return;
    };
    discovered.push(dir.to_path_buf());

    for module_name in extract_all_tag_contents(&pom_text, "module") {
        collect_modules(&dir.join(module_name.trim()), discovered);
    }
}

pub(crate) fn standard_source_dirs(module_dir: &Path) -> Vec<PathBuf> {
    ["src/main/java", "src/test/java"]
        .into_iter()
        .map(|relative| module_dir.join(relative))
        .filter(|dir| dir.is_dir())
        .collect()
}

fn resolve_java_version(root: &Path) -> Option<String> {
    [
        "maven.compiler.release",
        "maven.compiler.target",
        "maven.compiler.source",
    ]
    .into_iter()
    .find_map(|expression| evaluate_expression(root, expression))
}

fn evaluate_expression(root: &Path, expression: &str) -> Option<String> {
    let output = maven_command(root)
        .args([
            "-q",
            "-B",
            "help:evaluate",
            &format!("-Dexpression={expression}"),
            "-DforceStdout",
        ])
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() || text == "null object or invalid expression" {
        None
    } else {
        Some(text)
    }
}

fn read_classpath(path: &Path) -> Vec<PathBuf> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let separator = if cfg!(windows) { ';' } else { ':' };
    content
        .split(separator)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn run_maven(root: &Path, args: &[&str]) -> Result<(), String> {
    let output = maven_command(root)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|err| format!("failed to invoke mvn: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "mvn exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(())
}

fn maven_command(root: &Path) -> Command {
    let wrapper_name = if cfg!(windows) { "mvnw.cmd" } else { "mvnw" };
    let wrapper_path = root.join(wrapper_name);
    if wrapper_path.is_file() {
        Command::new(wrapper_path)
    } else {
        Command::new("mvn")
    }
}

fn extract_all_tag_contents(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut results = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find(&open) {
        let after_open = &rest[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        results.push(after_open[..end].to_string());
        rest = &after_open[end + close.len()..];
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    #[test]
    fn extract_all_tag_contents_finds_every_occurrence() {
        let xml = "<modules><module>lib</module><module>app</module></modules>";

        assert_eq!(
            extract_all_tag_contents(xml, "module"),
            vec!["lib".to_string(), "app".to_string()]
        );
    }

    #[test]
    fn extract_all_tag_contents_returns_empty_when_tag_is_absent() {
        assert!(extract_all_tag_contents("<project></project>", "module").is_empty());
    }

    #[test]
    fn discover_modules_finds_nested_modules_recursively() {
        let temp = TempDir::new("maven-discover");
        std::fs::write(
            temp.path.join("pom.xml"),
            "<project><modules><module>lib</module></modules></project>",
        )
        .unwrap();
        std::fs::create_dir_all(temp.path.join("lib")).unwrap();
        std::fs::write(temp.path.join("lib/pom.xml"), "<project></project>").unwrap();

        let modules = discover_modules(&temp.path);

        assert_eq!(modules, vec![temp.path.clone(), temp.path.join("lib")]);
    }

    #[test]
    fn read_classpath_splits_on_the_platform_separator() {
        let temp = TempDir::new("maven-classpath");
        let separator = if cfg!(windows) { ";" } else { ":" };
        std::fs::write(
            temp.path.join("cp.txt"),
            format!("/a/one.jar{separator}/a/two.jar"),
        )
        .unwrap();

        let classpath = read_classpath(&temp.path.join("cp.txt"));

        assert_eq!(
            classpath,
            vec![PathBuf::from("/a/one.jar"), PathBuf::from("/a/two.jar")]
        );
    }

    #[test]
    fn read_classpath_returns_empty_for_a_missing_file() {
        let temp = TempDir::new("maven-classpath");

        assert!(read_classpath(&temp.path.join("missing.txt")).is_empty());
    }

    #[test]
    fn resolves_the_real_maven_sample_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/maven_sample");

        let model = resolve_project_model(&root).expect("maven resolution should succeed");

        assert_eq!(model.modules.len(), 3);
        let app = model
            .modules
            .iter()
            .find(|m| m.root.ends_with("app"))
            .expect("app module present");
        let lib = model
            .modules
            .iter()
            .find(|m| m.root.ends_with("lib"))
            .expect("lib module present");

        assert!(
            app.source_dirs
                .iter()
                .any(|dir| dir.ends_with("app/src/main/java"))
        );
        assert_eq!(app.java_version.as_deref(), Some("17"));
        assert!(
            app.classpath
                .iter()
                .any(|entry| entry.file_name().is_some_and(|n| n == "gson-2.10.1.jar")),
            "app's classpath should transitively include gson: {:?}",
            app.classpath
        );
        assert!(
            app.classpath
                .iter()
                .any(|entry| entry.file_name().is_some_and(|n| n == "lib-1.0.0.jar")),
            "app's classpath should include lib's built jar: {:?}",
            app.classpath
        );
        // Verifies, rather than assumes, that Maven's default `dependency:
        // build-classpath` scope inclusion already surfaces a `provided`-scope
        // dependency (Lombok's conventional Maven setup) with no `maven.rs`
        // change needed.
        assert!(
            app.classpath.iter().any(|entry| {
                entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.to_lowercase().contains("lombok"))
            }),
            "app's classpath should include the provided-scope lombok dependency: {:?}",
            app.classpath
        );
        assert!(
            lib.classpath
                .iter()
                .any(|entry| entry.file_name().is_some_and(|n| n == "gson-2.10.1.jar")),
            "lib's classpath should include gson: {:?}",
            lib.classpath
        );
        // lib's pom.xml overrides maven.compiler.release to 21; app has no
        // override and inherits the root's 17.
        assert_eq!(lib.java_version.as_deref(), Some("21"));
    }
}
