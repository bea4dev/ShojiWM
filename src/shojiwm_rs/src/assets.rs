//! Where relative asset paths (shaders, images) are resolved from.
//!
//! The TypeScript runtime resolves them against the config package. A Rust
//! config is a compiled binary, so the root is chosen when it starts: the
//! value passed to [`ConfigBuilder::asset_root`](crate::ConfigBuilder::asset_root),
//! else `--runtime-dir`, else the directory of `--config`, else the current
//! directory.

use std::{
    cell::RefCell,
    path::{Component, Path, PathBuf},
};

thread_local! {
    static ASSET_ROOT: RefCell<PathBuf> = RefCell::new(PathBuf::from("."));
}

pub(crate) fn set_root(root: PathBuf) {
    ASSET_ROOT.with(|slot| *slot.borrow_mut() = root);
}

/// The directory relative asset paths are resolved against.
pub fn root() -> PathBuf {
    ASSET_ROOT.with(|slot| slot.borrow().clone())
}

/// Resolve `path` against [`root`]; absolute paths are returned unchanged.
pub fn resolve(path: &str) -> String {
    let path = Path::new(path);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root().join(path)
    };
    normalize(&joined).to_string_lossy().into_owned()
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_paths_against_the_root() {
        set_root(PathBuf::from("/config/pkg"));
        assert_eq!(
            resolve("./src/effect/a.frag"),
            "/config/pkg/src/effect/a.frag"
        );
        assert_eq!(resolve("../x.svg"), "/config/x.svg");
        assert_eq!(resolve("/abs/y.svg"), "/abs/y.svg");
    }
}
