//! Helpers for namespace-changing control writes.
//!
//! 9P servers can mark command files with the close-commit mode bit. Those
//! files commit when the handle is clunked, so the FUSE bridge defers kernel
//! invalidation until FLUSH/RELEASE has observed the close result. The bridge
//! must not infer this from server-specific path vocabulary.

pub(super) fn write_uses_control_timeout(path: &[Vec<u8>], close_commit: bool) -> bool {
    close_commit || is_control_file_name(path)
}

pub(super) fn close_commit_refreshes_namespace_bindings(close_commit: bool) -> bool {
    close_commit
}

pub(super) fn write_refreshes_namespace_bindings(path: &[Vec<u8>], close_commit: bool) -> bool {
    is_control_file_name(path) && !close_commit
}

pub(super) fn write_can_replay_after_namespace_refresh(
    path: &[Vec<u8>],
    close_commit: bool,
    offset: u64,
) -> bool {
    offset == 0 && (close_commit || is_control_file_name(path))
}

fn is_control_file_name(path: &[Vec<u8>]) -> bool {
    path.last()
        .map(|segment| segment.as_slice() == b"ctl")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        close_commit_refreshes_namespace_bindings, write_can_replay_after_namespace_refresh,
        write_refreshes_namespace_bindings, write_uses_control_timeout,
    };

    fn path(segments: &[&[u8]]) -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    }

    #[test]
    fn close_commit_writes_use_control_timeout_without_path_policy() {
        assert!(write_uses_control_timeout(
            &path(&[b"any", b"command"]),
            true
        ));
    }

    #[test]
    fn ctl_named_writes_use_control_timeout_without_namespace_policy() {
        assert!(write_uses_control_timeout(
            &path(&[b"entries", b"note", b"ctl"]),
            false
        ));
    }

    #[test]
    fn non_control_writes_use_regular_timeout() {
        assert!(!write_uses_control_timeout(
            &path(&[b"entries", b"note"]),
            false
        ));
    }

    #[test]
    fn close_commit_writes_defer_namespace_binding_refresh_until_close() {
        let command = path(&[b"any", b"command"]);

        assert!(!write_refreshes_namespace_bindings(&command, true));
        assert!(close_commit_refreshes_namespace_bindings(true));
        assert!(!close_commit_refreshes_namespace_bindings(false));
    }

    #[test]
    fn immediate_ctl_writes_refresh_namespace_bindings() {
        let ctl = path(&[b"entries", b"note", b"ctl"]);

        assert!(write_refreshes_namespace_bindings(&ctl, false));
        assert!(!write_refreshes_namespace_bindings(&ctl, true));
    }

    #[test]
    fn control_writes_can_replay_after_namespace_refresh_only_from_start() {
        let ctl = path(&[b"runtime", b"framework", b"candidates", b"full", b"ctl"]);
        let command = path(&[b"runtime", b"framework", b"reload", b"requests", b"new"]);
        let file = path(&[b"entries", b"note"]);

        assert!(write_can_replay_after_namespace_refresh(&ctl, false, 0));
        assert!(write_can_replay_after_namespace_refresh(&command, true, 0));
        assert!(!write_can_replay_after_namespace_refresh(&ctl, false, 1));
        assert!(!write_can_replay_after_namespace_refresh(&file, false, 0));
    }
}
