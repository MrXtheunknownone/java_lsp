//! Shells out to the real `javac` (with Lombok on `-processorpath`) to
//! compile a single Lombok-tagged source file — mirroring this project's
//! established precedent (M3's `gradle`/`mvn`, M4's `jimage`) of using a
//! real tool's ground-truth output rather than reimplementing its
//! internals. Lombok mutates javac's own in-memory AST during compilation,
//! so the resulting `.class` file already contains the synthesized
//! getters/setters as ordinary bytecode — no Lombok-specific decoding is
//! needed once compilation succeeds.

use crate::jdk_home::JdkHome;
use crate::project_model::ModuleModel;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Finds a Lombok jar on an already-resolved classpath by filename — the
/// same heuristic class of check `find_lombok_jar`'s callers already use
/// for "does this look like a build tool's own file" elsewhere in this
/// project (e.g. `jar`'s `.class`-suffix filtering).
pub fn find_lombok_jar(classpath: &[PathBuf]) -> Option<&PathBuf> {
    classpath.iter().find(|entry| {
        entry
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.to_lowercase().contains("lombok"))
    })
}

/// A stable, filesystem-safe output directory for a module's Lombok-driven
/// compiles, under the same cache-root convention M4 already established
/// for JDK/jar stub caching.
pub fn output_dir_for_module(cache_root: &Path, module_root: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    module_root.hash(&mut hasher);
    cache_root
        .join("javac-output")
        .join(format!("{:016x}", hasher.finish()))
}

/// The nine `--add-opens` flags Lombok needs to access `javac`'s internal
/// AST classes, sealed by module encapsulation since JDK 16. Each is
/// prefixed `-J` so it reaches the JVM running `javac`, not `javac`'s own
/// argument parser.
fn add_opens_flags() -> Vec<String> {
    const PACKAGES: [&str; 9] = [
        "code",
        "comp",
        "file",
        "main",
        "model",
        "parser",
        "processing",
        "tree",
        "util",
    ];
    PACKAGES
        .iter()
        .map(|package| {
            format!("-J--add-opens=jdk.compiler/com.sun.tools.javac.{package}=ALL-UNNAMED")
        })
        .collect()
}

fn join_paths(paths: &[PathBuf]) -> String {
    let separator = if cfg!(windows) { ';' } else { ':' };
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(&separator.to_string())
}

/// Builds the full `javac` argument list. Pure and independently testable —
/// `compile` is the only function here that actually spawns a process.
fn build_command_args(
    file: &Path,
    module: &ModuleModel,
    lombok_jar: &Path,
    output_dir: &Path,
    jdk_major_version: Option<u32>,
) -> Vec<String> {
    let mut args = Vec::new();

    // `--add-opens` is itself a JDK 9+ VM flag — passing it on an older JDK
    // would make the launching JVM refuse to start outright, rather than
    // just skip the Lombok-specific access it grants.
    if jdk_major_version.is_some_and(|version| version >= 9) {
        args.extend(add_opens_flags());
    }

    if !module.source_dirs.is_empty() {
        args.push("-sourcepath".to_string());
        args.push(join_paths(&module.source_dirs));
    }
    if !module.classpath.is_empty() {
        args.push("-cp".to_string());
        args.push(join_paths(&module.classpath));
    }

    args.push("-processorpath".to_string());
    args.push(lombok_jar.display().to_string());
    args.push("-d".to_string());
    args.push(output_dir.display().to_string());
    args.push(file.display().to_string());

    args
}

fn javac_executable_name() -> &'static str {
    if cfg!(windows) { "javac.exe" } else { "javac" }
}

/// Compiles `file` (the single Lombok-tagged compilation unit) with the
/// module's already-resolved source roots/classpath plus `lombok_jar` on
/// `-processorpath`, writing `.class` files to `output_dir`. `javac`
/// transparently compiles sibling source files it finds via `-sourcepath`
/// when a referenced type isn't already on the classpath — real,
/// documented `javac` behavior, not a guess — so cross-file references
/// within the same package resolve correctly without a whole-module
/// recompile.
pub fn compile(
    file: &Path,
    module: &ModuleModel,
    lombok_jar: &Path,
    output_dir: &Path,
    jdk: &JdkHome,
) -> Result<(), String> {
    std::fs::create_dir_all(output_dir).map_err(|err| format!("{output_dir:?}: {err}"))?;

    let javac = jdk.path.join("bin").join(javac_executable_name());
    let args = build_command_args(file, module, lombok_jar, output_dir, jdk.major_version);

    let output = Command::new(&javac)
        .args(&args)
        .output()
        .map_err(|err| format!("failed to run {javac:?}: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "{javac:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{classfile, gradle, jdk_home};
    use std::path::Path;

    fn module(source_dirs: Vec<PathBuf>, classpath: Vec<PathBuf>) -> ModuleModel {
        ModuleModel {
            root: PathBuf::from("/module"),
            source_dirs,
            classpath,
            java_version: None,
        }
    }

    #[test]
    fn find_lombok_jar_finds_a_jar_by_filename() {
        let classpath = vec![
            PathBuf::from("/repo/.gradle/gson-2.10.1.jar"),
            PathBuf::from("/repo/.gradle/lombok-1.18.46.jar"),
        ];

        let found = find_lombok_jar(&classpath).unwrap();

        assert_eq!(found, &PathBuf::from("/repo/.gradle/lombok-1.18.46.jar"));
    }

    #[test]
    fn find_lombok_jar_is_case_insensitive() {
        let classpath = vec![PathBuf::from("/repo/.gradle/LOMBOK-1.18.46.jar")];

        assert!(find_lombok_jar(&classpath).is_some());
    }

    #[test]
    fn find_lombok_jar_returns_none_when_absent() {
        let classpath = vec![PathBuf::from("/repo/.gradle/gson-2.10.1.jar")];

        assert!(find_lombok_jar(&classpath).is_none());
    }

    #[test]
    fn add_opens_flags_covers_all_nine_javac_internal_packages() {
        let flags = add_opens_flags();

        assert_eq!(flags.len(), 9);
        for flag in &flags {
            assert!(flag.starts_with("-J--add-opens=jdk.compiler/com.sun.tools.javac."));
            assert!(flag.ends_with("=ALL-UNNAMED"));
        }
    }

    #[test]
    fn build_command_args_omits_add_opens_on_a_pre_jdk9_version() {
        let args = build_command_args(
            Path::new("/src/Widget.java"),
            &module(vec![], vec![]),
            Path::new("/lombok.jar"),
            Path::new("/out"),
            Some(8),
        );

        assert!(!args.iter().any(|a| a.starts_with("-J--add-opens")));
    }

    #[test]
    fn build_command_args_includes_add_opens_on_jdk9_and_newer() {
        let args = build_command_args(
            Path::new("/src/Widget.java"),
            &module(vec![], vec![]),
            Path::new("/lombok.jar"),
            Path::new("/out"),
            Some(21),
        );

        assert_eq!(
            args.iter()
                .filter(|a| a.starts_with("-J--add-opens"))
                .count(),
            9
        );
    }

    #[test]
    fn build_command_args_includes_sourcepath_classpath_processorpath_and_output() {
        let args = build_command_args(
            Path::new("/src/Widget.java"),
            &module(vec![PathBuf::from("/src")], vec![PathBuf::from("/dep.jar")]),
            Path::new("/lombok.jar"),
            Path::new("/out"),
            None,
        );

        assert!(args.windows(2).any(|w| w == ["-sourcepath", "/src"]));
        assert!(args.windows(2).any(|w| w == ["-cp", "/dep.jar"]));
        assert!(
            args.windows(2)
                .any(|w| w == ["-processorpath", "/lombok.jar"])
        );
        assert!(args.windows(2).any(|w| w == ["-d", "/out"]));
        assert_eq!(args.last().unwrap(), "/src/Widget.java");
    }

    #[test]
    fn output_dir_for_module_is_deterministic_and_distinguishes_modules() {
        let cache_root = Path::new("/cache");

        let a1 = output_dir_for_module(cache_root, Path::new("/repo/app"));
        let a2 = output_dir_for_module(cache_root, Path::new("/repo/app"));
        let b = output_dir_for_module(cache_root, Path::new("/repo/lib"));

        assert_eq!(a1, a2);
        assert_ne!(a1, b);
    }

    #[test]
    fn compiling_a_real_lombok_annotated_class_produces_a_generated_getter() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lombok_sample");
        let model = gradle::resolve_project_model(&root).expect("gradle resolution should succeed");
        let module = &model.modules[0];
        let lombok_jar = find_lombok_jar(&module.classpath)
            .expect("lombok_sample's classpath should include a lombok jar");
        let jdk =
            jdk_home::locate().expect("a real JDK should be locatable in this dev environment");
        let file = root.join("src/main/java/com/example/lombok/Widget.java");
        let output_dir = crate::test_support::TempDir::new("javac-compile-lombok");

        compile(&file, module, lombok_jar, &output_dir.path, &jdk).unwrap();

        let class_bytes =
            std::fs::read(output_dir.path.join("com/example/lombok/Widget.class")).unwrap();
        let class = classfile::parse(&class_bytes).unwrap();
        assert!(
            class.methods.iter().any(|m| m.name == "getName"),
            "expected a Lombok-generated getName method, got: {:?}",
            class.methods.iter().map(|m| &m.name).collect::<Vec<_>>()
        );
    }
}
