use super::{parse_namespace_path, protocol_error, render_namespace_path};
use crate::{Error, Result};
use r9p::{
    qid::{Qid, DMDIR},
    stat::{dirread_chunk, Stat},
};
use std::collections::BTreeSet;

const SYNTHETIC_QID_BIT: u64 = 1 << 63;
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

pub(super) fn is_ancestor(
    namespace_path: &[Vec<u8>],
    mount_paths: impl IntoIterator<Item = Vec<u8>>,
) -> bool {
    mount_paths
        .into_iter()
        .any(|mount_path| immediate_child(namespace_path, &mount_path).is_some())
}

pub(super) fn stat(namespace_path: &[Vec<u8>]) -> Stat {
    let name = namespace_path
        .last()
        .cloned()
        .unwrap_or_else(|| b".".to_vec());
    Stat::new(name, qid(namespace_path), DMDIR | 0o555)
}

pub(super) fn read(
    namespace_path: &[Vec<u8>],
    mount_paths: impl IntoIterator<Item = Vec<u8>>,
    offset: u64,
    count: u32,
) -> Result<Vec<u8>> {
    let names = mount_paths
        .into_iter()
        .filter_map(|mount_path| immediate_child(namespace_path, &mount_path))
        .collect::<BTreeSet<_>>();
    let entries = names
        .into_iter()
        .map(|name| {
            let mut child_path = namespace_path.to_vec();
            child_path.push(name.clone());
            Stat::new(name, qid(&child_path), DMDIR | 0o555)
        })
        .collect::<Vec<_>>();
    dirread_chunk(&entries, offset, count).map_err(protocol_error)
}

fn immediate_child(namespace_path: &[Vec<u8>], mount_path: &[u8]) -> Option<Vec<u8>> {
    let mount = parse_namespace_path(mount_path).ok()?;
    if namespace_path.len() >= mount.len()
        || !namespace_path
            .iter()
            .zip(&mount)
            .all(|(left, right)| left == right)
    {
        return None;
    }
    Some(mount[namespace_path.len()].clone())
}

fn qid(namespace_path: &[Vec<u8>]) -> Qid {
    let path = render_namespace_path(namespace_path);
    let hash = path.iter().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    });
    Qid::dir(hash | SYNTHETIC_QID_BIT)
}

pub(super) fn read_only_error() -> Error {
    Error::new(libc::EROFS, "namespace referral directories are read-only")
}

pub(super) fn directory_operation_error() -> Error {
    Error::new(
        libc::EISDIR,
        "operation is not valid on a namespace referral directory",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use r9p::stat::decode_dir_entries;

    #[test]
    fn exposes_only_immediate_unique_children() {
        let path = vec![b"agents".to_vec()];
        let data = read(
            &path,
            [
                b"/agents/runtime/m7".to_vec(),
                b"/agents/runtime/nucbox".to_vec(),
                b"/agents/profile/default".to_vec(),
                b"/other".to_vec(),
            ],
            0,
            u32::MAX,
        )
        .expect("synthetic directory should encode");
        let entries = decode_dir_entries(&data).expect("directory entries should decode");
        assert_eq!(
            entries
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            vec![b"profile".to_vec(), b"runtime".to_vec()]
        );
    }

    #[test]
    fn does_not_confuse_path_prefixes() {
        assert!(is_ancestor(
            &[b"agents".to_vec()],
            [b"/agents/runtime/m7".to_vec()]
        ));
        assert!(!is_ancestor(
            &[b"agent".to_vec()],
            [b"/agents/runtime/m7".to_vec()]
        ));
    }
}
