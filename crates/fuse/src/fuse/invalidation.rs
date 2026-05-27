//! Kernel cache invalidation helpers shared by namespace-change paths.

use super::reply::{notify_inval_entry, notify_inval_inode};
use crate::node::{StaleBinding, ROOT_NODEID};
use std::{collections::BTreeSet, fs::File};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KernelInvalidation {
    pub(super) stale_bindings: Vec<StaleBinding>,
    pub(super) parent_entries: Vec<(u64, Vec<u8>)>,
    pub(super) coarse: bool,
}

impl KernelInvalidation {
    pub(super) fn path(
        stale_bindings: Vec<StaleBinding>,
        parent_entries: Vec<(u64, Vec<u8>)>,
    ) -> Self {
        Self {
            stale_bindings,
            parent_entries,
            coarse: false,
        }
    }

    pub(super) fn coarse(stale_bindings: Vec<StaleBinding>) -> Self {
        Self {
            stale_bindings,
            parent_entries: Vec::new(),
            coarse: true,
        }
    }
}

pub(super) fn notify_kernel_invalidations(file: &mut File, invalidation: &KernelInvalidation) {
    for nodeid in directory_nodeids_for_readdir_cache(invalidation) {
        let _ = notify_inval_inode(file, nodeid);
    }
    for binding in &invalidation.stale_bindings {
        let _ = notify_inval_inode(file, binding.nodeid);
        if let Some(parent_nodeid) = binding.parent_nodeid {
            let _ = notify_inval_entry(file, parent_nodeid, &binding.name);
        }
    }
    for (parent, name) in &invalidation.parent_entries {
        let _ = notify_inval_entry(file, *parent, name);
    }
}

fn directory_nodeids_for_readdir_cache(invalidation: &KernelInvalidation) -> Vec<u64> {
    let mut nodeids = BTreeSet::new();
    if invalidation.coarse {
        nodeids.insert(ROOT_NODEID);
    }
    for binding in &invalidation.stale_bindings {
        if let Some(parent_nodeid) = binding.parent_nodeid {
            nodeids.insert(parent_nodeid);
        }
    }
    for (parent_nodeid, _) in &invalidation.parent_entries {
        nodeids.insert(*parent_nodeid);
    }
    nodeids.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::{directory_nodeids_for_readdir_cache, KernelInvalidation};
    use crate::node::{StaleBinding, ROOT_NODEID};

    #[test]
    fn coarse_invalidation_includes_root_even_without_stale_children() {
        let invalidation = KernelInvalidation::coarse(Vec::new());

        assert_eq!(
            directory_nodeids_for_readdir_cache(&invalidation),
            vec![ROOT_NODEID]
        );
    }

    #[test]
    fn stale_bindings_invalidate_parent_directory_readdir_caches() {
        let invalidation = KernelInvalidation::coarse(vec![
            StaleBinding {
                nodeid: 10,
                parent_nodeid: Some(ROOT_NODEID),
                name: b"calendar".to_vec(),
                fid: Some(2),
            },
            StaleBinding {
                nodeid: 11,
                parent_nodeid: Some(10),
                name: b"commands".to_vec(),
                fid: Some(3),
            },
        ]);

        assert_eq!(
            directory_nodeids_for_readdir_cache(&invalidation),
            vec![ROOT_NODEID, 10]
        );
    }

    #[test]
    fn created_path_invalidation_includes_parent_without_child_node() {
        let invalidation = KernelInvalidation::path(Vec::new(), vec![(42, b"put-event".to_vec())]);

        assert_eq!(directory_nodeids_for_readdir_cache(&invalidation), vec![42]);
    }
}
