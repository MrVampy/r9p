//! Helpers for namespace-changing control writes.
//!
//! Vault command files with the close-commit mode bit only change namespace
//! state when the handle is clunked. The FUSE bridge therefore defers kernel
//! invalidation for those writes until FLUSH/RELEASE has committed.

pub(super) fn is_runtime_control_write_path(path: &[Vec<u8>]) -> bool {
    path_matches(path, &[b"runtime", b"worktrees", b"import"])
        || (path.len() >= 2
            && path[0].as_slice() == b"runtime"
            && path
                .last()
                .map(|segment| segment.as_slice() == b"ctl")
                .unwrap_or(false))
}

pub(super) fn refreshes_namespace_bindings(path: &[Vec<u8>]) -> bool {
    path_matches(path, &[b"runtime", b"namespaces", b"current", b"mount"])
        || path_matches(path, &[b"runtime", b"namespaces", b"current", b"unmount"])
        || path_matches(path, &[b"runtime", b"worktrees", b"import"])
        || is_worktree_control_write_path(path)
}

pub(super) fn write_refreshes_namespace_bindings(path: &[Vec<u8>], close_commit: bool) -> bool {
    refreshes_namespace_bindings(path) && !close_commit
}

fn is_worktree_control_write_path(path: &[Vec<u8>]) -> bool {
    path.len() == 4
        && path[0].as_slice() == b"runtime"
        && path[1].as_slice() == b"worktrees"
        && !path[2].is_empty()
        && path[3].as_slice() == b"ctl"
}

fn path_matches(path: &[Vec<u8>], expected: &[&[u8]]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected.iter())
            .all(|(left, right)| left.as_slice() == *right)
}

#[cfg(test)]
mod tests {
    use super::{
        is_runtime_control_write_path, refreshes_namespace_bindings,
        write_refreshes_namespace_bindings,
    };

    fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    }

    #[test]
    fn worktree_ctl_writes_refresh_namespace_bindings() {
        assert!(refreshes_namespace_bindings(&path(&[
            b"runtime",
            b"worktrees",
            b"wt-plan45",
            b"ctl",
        ])));
    }

    #[test]
    fn worktree_import_writes_refresh_namespace_bindings() {
        assert!(refreshes_namespace_bindings(&path(&[
            b"runtime",
            b"worktrees",
            b"import",
        ])));
    }

    #[test]
    fn worktree_children_do_not_all_refresh_namespace_bindings() {
        assert!(!refreshes_namespace_bindings(&path(&[
            b"runtime",
            b"worktrees",
            b"wt-plan45",
            b"status",
        ])));
    }

    #[test]
    fn close_commit_writes_defer_namespace_binding_refresh_until_close() {
        let import = path(&[b"runtime", b"worktrees", b"import"]);

        assert!(write_refreshes_namespace_bindings(&import, false));
        assert!(!write_refreshes_namespace_bindings(&import, true));
        assert!(refreshes_namespace_bindings(&import));
    }

    #[test]
    fn runtime_ctl_writes_use_control_timeout() {
        assert!(is_runtime_control_write_path(&path(&[
            b"runtime",
            b"framework",
            b"reload",
            b"ctl",
        ])));
        assert!(is_runtime_control_write_path(&path(&[
            b"runtime",
            b"framework",
            b"staging",
            b"providers",
            b"current",
            b"ctl",
        ])));
        assert!(is_runtime_control_write_path(&path(&[
            b"runtime",
            b"services",
            b"r9p-listener",
            b"ctl",
        ])));
        assert!(is_runtime_control_write_path(&path(&[
            b"runtime",
            b"worktrees",
            b"import",
        ])));
    }

    #[test]
    fn non_runtime_ctl_writes_use_regular_timeout() {
        assert!(!is_runtime_control_write_path(&path(&[
            b"entries", b"note", b"ctl",
        ])));
    }
}
