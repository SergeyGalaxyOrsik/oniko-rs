//! The Cargo build folder that every task agent of a project shares, `<project>/target`.

use std::path::{Path, PathBuf};

pub fn target_dir(project: &str) -> PathBuf {
    Path::new(project).join("target")
}

/// The bytes in the project's build folder; None when the project is not a Cargo project.
pub fn target_size(project: &str) -> Option<u64> {
    Path::new(project).join("Cargo.toml").is_file().then(|| folder_size(&target_dir(project)))
}

/// Symlinks count as themselves, so a link out of the folder is not followed.
fn folder_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries.flatten().map(|entry| folder_size(&entry.path())).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_the_target_folder_of_a_cargo_project_only() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug/deps")).unwrap();
        std::fs::write(dir.path().join("target/debug/deps/a.rlib"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.path().join("target/CACHEDIR.TAG"), vec![0u8; 200]).unwrap();
        std::fs::write(dir.path().join("big-outside.bin"), vec![0u8; 50_000]).unwrap();
        assert_eq!(target_size(project), None, "no Cargo.toml");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        assert_eq!(target_size(project), Some(3200));
        std::os::unix::fs::symlink(dir.path().join("big-outside.bin"), dir.path().join("target/link")).unwrap();
        let link_len = std::fs::symlink_metadata(dir.path().join("target/link")).unwrap().len();
        assert_eq!(target_size(project), Some(3200 + link_len));
        std::fs::remove_dir_all(dir.path().join("target")).unwrap();
        assert_eq!(target_size(project), Some(0));
    }
}
