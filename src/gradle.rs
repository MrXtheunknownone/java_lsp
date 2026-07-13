use crate::project_model::{ModuleModel, ProjectModel};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const INIT_SCRIPT: &str = r#"
allprojects {
    afterEvaluate { project ->
        tasks.register("javaLspProjectModel") {
            doLast {
                if (project.plugins.hasPlugin('java')) {
                    def sourceSets = project.sourceSets
                    println "JAVA_LSP_PROJECT ${project.projectDir}"
                    sourceSets.each { ss ->
                        ss.java.srcDirs.each { dir ->
                            println "JAVA_LSP_SOURCE_DIR ${dir}"
                        }
                    }
                    println "JAVA_LSP_JAVA_VERSION ${project.java.sourceCompatibility}"
                    sourceSets.main.runtimeClasspath.files.each { f ->
                        println "JAVA_LSP_CLASSPATH_ENTRY ${f}"
                    }
                    // Lombok's recommended Gradle setup (`compileOnly` +
                    // `annotationProcessor`) keeps its jar off the runtime
                    // classpath above, so surface it separately.
                    project.configurations.findByName('annotationProcessor')?.files?.each { f ->
                        println "JAVA_LSP_CLASSPATH_ENTRY ${f}"
                    }
                }
            }
        }
    }
}
"#;

/// Distinguishes concurrent invocations from the same process — every
/// gradle-invoking test shares one `process::id()`, so that alone isn't a
/// unique temp-file name; two concurrent invocations racing on the same
/// init-script path could see one's cleanup delete the file the other is
/// still reading.
static INIT_SCRIPT_COUNTER: AtomicU32 = AtomicU32::new(0);

pub fn resolve_project_model(root: &Path) -> Result<ProjectModel, String> {
    let invocation_id = INIT_SCRIPT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let init_script_path = std::env::temp_dir().join(format!(
        "java-lsp-gradle-init-{}-{invocation_id}.gradle",
        std::process::id()
    ));
    std::fs::write(&init_script_path, INIT_SCRIPT)
        .map_err(|err| format!("failed to write Gradle init script: {err}"))?;

    let output = gradle_command(root)
        .arg("--console=plain")
        .arg("-q")
        .arg("--init-script")
        .arg(&init_script_path)
        .arg("javaLspProjectModel")
        .current_dir(root)
        .output();

    let _ = std::fs::remove_file(&init_script_path);
    let output = output.map_err(|err| format!("failed to invoke gradle: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "gradle exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(parse_project_model(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn gradle_command(root: &Path) -> Command {
    let wrapper_name = if cfg!(windows) {
        "gradlew.bat"
    } else {
        "gradlew"
    };
    let wrapper_path = root.join(wrapper_name);
    if wrapper_path.is_file() {
        Command::new(wrapper_path)
    } else {
        Command::new("gradle")
    }
}

fn parse_project_model(stdout: &str) -> ProjectModel {
    let mut modules = Vec::new();
    let mut current: Option<ModuleModel> = None;

    for line in stdout.lines() {
        if let Some(dir) = line.strip_prefix("JAVA_LSP_PROJECT ") {
            modules.extend(current.take());
            current = Some(ModuleModel {
                root: PathBuf::from(dir),
                source_dirs: Vec::new(),
                classpath: Vec::new(),
                java_version: None,
            });
        } else if let Some(dir) = line.strip_prefix("JAVA_LSP_SOURCE_DIR ")
            && let Some(module) = current.as_mut()
        {
            module.source_dirs.push(PathBuf::from(dir));
        } else if let Some(version) = line.strip_prefix("JAVA_LSP_JAVA_VERSION ")
            && let Some(module) = current.as_mut()
        {
            module.java_version = Some(version.to_string());
        } else if let Some(entry) = line.strip_prefix("JAVA_LSP_CLASSPATH_ENTRY ")
            && let Some(module) = current.as_mut()
        {
            module.classpath.push(PathBuf::from(entry));
        }
    }
    modules.extend(current);

    ProjectModel { modules }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_project_model_splits_output_into_per_module_blocks() {
        let stdout = "\
JAVA_LSP_PROJECT /root/app
JAVA_LSP_SOURCE_DIR /root/app/src/main/java
JAVA_LSP_JAVA_VERSION 17
JAVA_LSP_CLASSPATH_ENTRY /root/app/build/classes/java/main
JAVA_LSP_CLASSPATH_ENTRY /root/lib.jar
JAVA_LSP_PROJECT /root/lib
JAVA_LSP_SOURCE_DIR /root/lib/src/main/java
JAVA_LSP_JAVA_VERSION 17
JAVA_LSP_CLASSPATH_ENTRY /root/gson.jar
";

        let model = parse_project_model(stdout);

        assert_eq!(model.modules.len(), 2);
        assert_eq!(model.modules[0].root, PathBuf::from("/root/app"));
        assert_eq!(
            model.modules[0].source_dirs,
            vec![PathBuf::from("/root/app/src/main/java")]
        );
        assert_eq!(model.modules[0].java_version, Some("17".to_string()));
        assert_eq!(
            model.modules[0].classpath,
            vec![
                PathBuf::from("/root/app/build/classes/java/main"),
                PathBuf::from("/root/lib.jar"),
            ]
        );
        assert_eq!(model.modules[1].root, PathBuf::from("/root/lib"));
        assert_eq!(
            model.modules[1].classpath,
            vec![PathBuf::from("/root/gson.jar")]
        );
    }

    #[test]
    fn parse_project_model_returns_no_modules_for_empty_output() {
        let model = parse_project_model("");

        assert!(model.modules.is_empty());
    }

    #[test]
    fn resolves_the_real_gradle_sample_fixture() {
        let _guard = crate::test_support::GRADLE_SAMPLE_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gradle_sample");

        let model = resolve_project_model(&root).expect("gradle resolution should succeed");

        assert_eq!(model.modules.len(), 2);
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
            app.classpath.iter().any(|entry| {
                entry
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.to_lowercase().contains("lombok"))
            }),
            "app's classpath should include lombok via the annotationProcessor configuration: {:?}",
            app.classpath
        );
        assert!(
            app.classpath
                .iter()
                .any(|entry| entry.file_name().is_some_and(|n| n == "lib.jar")),
            "app's classpath should include lib's built jar: {:?}",
            app.classpath
        );
        assert!(
            lib.classpath
                .iter()
                .any(|entry| entry.file_name().is_some_and(|n| n == "gson-2.10.1.jar")),
            "lib's classpath should include gson: {:?}",
            lib.classpath
        );
    }
}
