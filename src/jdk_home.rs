//! Locates the JDK whose classes should be indexed for Tier 3b. One JDK for
//! the whole server process — per-module JDK selection is out of scope for
//! M4, mirroring how `ProjectModel::java_version` is already just an
//! unenforced loose string rather than something the server acts on.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JdkHome {
    pub path: PathBuf,
    pub major_version: Option<u32>,
}

/// Checks `JAVA_HOME` first, then falls back to scanning `PATH` for a
/// `java`/`java.exe` executable and taking its bin directory's parent.
pub fn locate() -> Option<JdkHome> {
    locate_with(
        std::env::var_os("JAVA_HOME"),
        std::env::var_os("PATH").as_deref(),
    )
}

fn locate_with(java_home: Option<std::ffi::OsString>, path_var: Option<&OsStr>) -> Option<JdkHome> {
    let home = java_home
        .map(PathBuf::from)
        .filter(|path| looks_like_jdk_home(path))
        .or_else(|| locate_from_path(path_var))?;

    Some(JdkHome {
        major_version: read_major_version(&home),
        path: home,
    })
}

fn java_executable_name() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

fn looks_like_jdk_home(path: &Path) -> bool {
    path.join("bin").join(java_executable_name()).is_file()
}

/// Scans `PATH` for a `java` executable and returns the JDK home it belongs
/// to. Resolves symlinks first: on many Linux distributions, `PATH` finds
/// `java` via a generic system directory (`/usr/bin`, `/usr/sbin`) that
/// merely symlinks into the real JDK install — the *canonical* path's
/// grandparent is the real home, not the symlink's.
fn locate_from_path(path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var).find_map(|dir| {
        let java = dir.join(java_executable_name());
        if !java.is_file() {
            return None;
        }
        let canonical = std::fs::canonicalize(&java).unwrap_or(java);
        canonical.parent()?.parent().map(Path::to_path_buf)
    })
}

fn read_major_version(home: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(home.join("release")).ok()?;
    let line = content
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?;
    parse_java_version(line.trim_matches('"'))
}

/// Extracts the major version from a JDK version string, handling both the
/// modern scheme (`"21.0.11"` -> 21) and the pre-JDK-9 `"1.x"` scheme
/// (`"1.8.0_292"` -> 8).
fn parse_java_version(value: &str) -> Option<u32> {
    let value = value.strip_prefix("1.").unwrap_or(value);
    let digits_end = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    value[..digits_end].parse().ok()
}

/// Where to find this JDK's classfiles: JDK 9+ stores them in `lib/modules`
/// (a custom `jimage` binary format, not a zip), so `java.base` is extracted
/// once via the JDK's own bundled `jimage` tool — mirroring M3's precedent
/// of shelling out to `gradle`/`mvn` rather than reimplementing a build
/// tool's internals — and cached on disk. Pre-JDK-9 `rt.jar` is a plain
/// jar, read directly via `jar::class_entries`, no extraction needed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassSource {
    ExtractedDir(PathBuf),
    Jar(PathBuf),
}

pub fn class_source(jdk: &JdkHome, cache_root: &Path) -> Result<ClassSource, String> {
    let rt_jar = jdk.path.join("lib").join("rt.jar");
    if jdk.major_version.is_some_and(|version| version < 9) {
        return Ok(ClassSource::Jar(rt_jar));
    }

    let modules_file = jdk.path.join("lib").join("modules");
    if !modules_file.is_file() {
        return Ok(ClassSource::Jar(rt_jar));
    }

    extract_java_base(jdk, &modules_file, cache_root).map(ClassSource::ExtractedDir)
}

fn extract_java_base(
    jdk: &JdkHome,
    modules_file: &Path,
    cache_root: &Path,
) -> Result<PathBuf, String> {
    let target_dir = cache_root
        .join("jdk")
        .join(cache_key(&jdk.path, modules_file));
    let java_base_dir = target_dir.join("java.base");

    if java_base_dir.is_dir() {
        return Ok(java_base_dir);
    }

    std::fs::create_dir_all(&target_dir).map_err(|err| format!("{target_dir:?}: {err}"))?;

    let jimage = jdk.path.join("bin").join(jimage_executable_name());
    let output = std::process::Command::new(&jimage)
        .arg("extract")
        .arg("--dir")
        .arg(&target_dir)
        .arg("--include")
        .arg("regex:/java.base/.*")
        .arg(modules_file)
        .output()
        .map_err(|err| format!("failed to run {jimage:?}: {err}"))?;

    if !output.status.success() {
        return Err(format!(
            "{jimage:?} extract failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(java_base_dir)
}

fn jimage_executable_name() -> &'static str {
    if cfg!(windows) {
        "jimage.exe"
    } else {
        "jimage"
    }
}

/// A non-cryptographic cache key: a hash collision here would only cause a
/// stale-cache reuse for classfile symbol data, not a security or
/// correctness issue worth a cryptographic hash dependency.
fn cache_key(jdk_path: &Path, modules_file: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mtime = std::fs::metadata(modules_file)
        .and_then(|m| m.modified())
        .ok();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    jdk_path.hash(&mut hasher);
    mtime.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::ffi::OsString;

    fn make_fake_jdk_home(release_version: Option<&str>) -> TempDir {
        let temp = TempDir::new("jdk-home");
        std::fs::create_dir_all(temp.path.join("bin")).unwrap();
        std::fs::write(temp.path.join("bin").join(java_executable_name()), b"").unwrap();
        if let Some(version) = release_version {
            std::fs::write(
                temp.path.join("release"),
                format!("IMPLEMENTOR=\"Test\"\nJAVA_VERSION=\"{version}\"\n"),
            )
            .unwrap();
        }
        temp
    }

    #[test]
    fn locate_with_prefers_a_valid_java_home() {
        let fake_home = make_fake_jdk_home(Some("21.0.9"));

        let jdk = locate_with(Some(OsString::from(&fake_home.path)), None).unwrap();

        assert_eq!(jdk.path, fake_home.path);
        assert_eq!(jdk.major_version, Some(21));
    }

    #[test]
    fn locate_with_falls_back_to_path_when_java_home_is_invalid() {
        let fake_home = make_fake_jdk_home(Some("17.0.2"));
        let bin_dir = fake_home.path.join("bin");
        let path_var = std::env::join_paths([&bin_dir]).unwrap();

        let jdk = locate_with(
            Some(OsString::from("/does/not/exist")),
            Some(path_var.as_os_str()),
        )
        .unwrap();

        assert_eq!(jdk.path, fake_home.path);
        assert_eq!(jdk.major_version, Some(17));
    }

    #[test]
    fn locate_with_falls_back_to_path_when_java_home_is_unset() {
        let fake_home = make_fake_jdk_home(Some("11.0.1"));
        let bin_dir = fake_home.path.join("bin");
        let path_var = std::env::join_paths([&bin_dir]).unwrap();

        let jdk = locate_with(None, Some(path_var.as_os_str())).unwrap();

        assert_eq!(jdk.path, fake_home.path);
    }

    #[test]
    fn locate_with_returns_none_when_nothing_looks_like_a_jdk() {
        let empty = TempDir::new("not-a-jdk");
        let path_var = std::env::join_paths([&empty.path]).unwrap();

        let jdk = locate_with(
            Some(OsString::from("/does/not/exist")),
            Some(path_var.as_os_str()),
        );

        assert!(jdk.is_none());
    }

    #[test]
    fn locate_with_tolerates_a_missing_release_file() {
        let fake_home = make_fake_jdk_home(None);

        let jdk = locate_with(Some(OsString::from(&fake_home.path)), None).unwrap();

        assert_eq!(jdk.major_version, None);
    }

    #[test]
    fn parse_java_version_reads_the_modern_scheme() {
        assert_eq!(parse_java_version("21.0.11"), Some(21));
        assert_eq!(parse_java_version("17"), Some(17));
    }

    #[test]
    fn parse_java_version_reads_the_pre_jdk9_scheme() {
        assert_eq!(parse_java_version("1.8.0_292"), Some(8));
    }

    #[test]
    fn locate_finds_the_real_jdk_on_this_machine() {
        let jdk = locate().expect("a real JDK should be locatable in this dev environment");

        assert!(jdk.path.join("bin").join(java_executable_name()).is_file());
        assert!(jdk.major_version.is_some());
    }

    #[test]
    fn class_source_returns_rt_jar_for_a_pre_jdk9_home() {
        let jdk = JdkHome {
            path: PathBuf::from("/fake/jdk8"),
            major_version: Some(8),
        };
        let cache_root = TempDir::new("class-source-jdk8");

        let source = class_source(&jdk, &cache_root.path).unwrap();

        assert_eq!(
            source,
            ClassSource::Jar(PathBuf::from("/fake/jdk8/lib/rt.jar"))
        );
    }

    #[test]
    fn class_source_falls_back_to_rt_jar_when_modules_file_is_absent() {
        let fake_home = make_fake_jdk_home(Some("21.0.9"));
        let jdk = JdkHome {
            path: fake_home.path.clone(),
            major_version: Some(21),
        };
        let cache_root = TempDir::new("class-source-no-modules");

        let source = class_source(&jdk, &cache_root.path).unwrap();

        assert_eq!(
            source,
            ClassSource::Jar(fake_home.path.join("lib").join("rt.jar"))
        );
    }

    #[test]
    fn class_source_extracts_java_base_for_the_real_jdk() {
        let jdk = locate().expect("a real JDK should be locatable in this dev environment");
        let cache_root = TempDir::new("class-source-real-jdk");

        let source = class_source(&jdk, &cache_root.path).unwrap();

        let ClassSource::ExtractedDir(dir) = source else {
            panic!("expected java.base to be extracted for a modern JDK");
        };
        assert!(dir.join("java/util/List.class").is_file());
    }

    #[test]
    fn class_source_reuses_a_warm_cache_instead_of_re_extracting() {
        let jdk = locate().expect("a real JDK should be locatable in this dev environment");
        let cache_root = TempDir::new("class-source-cache-hit");

        let first = class_source(&jdk, &cache_root.path).unwrap();
        let ClassSource::ExtractedDir(dir) = &first else {
            panic!("expected java.base to be extracted for a modern JDK");
        };
        let list_class = dir.join("java/util/List.class");
        assert!(list_class.is_file());
        std::fs::remove_file(&list_class).unwrap();

        let second = class_source(&jdk, &cache_root.path).unwrap();

        assert_eq!(first, second);
        assert!(
            !list_class.is_file(),
            "a cache hit must not re-run jimage extract"
        );
    }
}
