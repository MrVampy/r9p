use super::*;
use crate::model::{ChildDirectoryReply, ChildDirectoryResolver};

impl Front {
    pub fn register_child_directory_resolver(
        &self,
        path: &str,
        resolution_prefix: &str,
        read_prefix: &str,
        removal: ChildDirectoryRemoval,
    ) -> Result<()> {
        let path = path.trim_matches('/');
        if path.is_empty() {
            return Err(Error::from_static(EPERM));
        }
        let resolution_prefix = normalise_request_prefix(resolution_prefix)?;
        let read_prefix = normalise_request_prefix(read_prefix)?;
        if resolution_prefix == read_prefix {
            return Err(Error::from_static(
                "child resolution and directory read prefixes must be distinct",
            ));
        }
        let mut state = self.lock()?;
        let id = state.ensure_path_dir(path)?;
        let node = state
            .nodes
            .get_mut(&id)
            .ok_or_else(|| Error::from_static(ENOENT))?;
        let Body::Dir(directory) = &mut node.body else {
            return Err(Error::from_static(ENOTDIR));
        };
        directory.child_resolver = Some(ChildDirectoryResolver {
            resolution_prefix,
            read_prefix,
            removal,
        });
        Ok(())
    }

    pub fn complete_child_directory_resolution(
        &self,
        prefix: &str,
        request_id: u64,
        metadata: PushedDirectoryMetadata,
    ) -> Result<()> {
        if metadata.length != 0 {
            return Err(Error::from_static("pushed directory length must be zero"));
        }
        self.complete_child_directory_resolution_result(
            prefix,
            request_id,
            ChildDirectoryReply::Accepted(metadata),
        )
    }

    pub fn reject_child_directory_resolution(
        &self,
        prefix: &str,
        request_id: u64,
        message: &str,
    ) -> Result<()> {
        self.complete_child_directory_resolution_result(
            prefix,
            request_id,
            ChildDirectoryReply::Rejected(message.to_string()),
        )
    }

    fn complete_child_directory_resolution_result(
        &self,
        prefix: &str,
        request_id: u64,
        reply: ChildDirectoryReply,
    ) -> Result<()> {
        let prefix = normalise_request_prefix(prefix)?;
        let mut state = self.lock()?;
        let key = state
            .child_directory_resolution_requests
            .get(&request_id)
            .cloned()
            .ok_or_else(|| Error::from_static(ENOENT))?;
        let resolution = state
            .child_directory_resolutions
            .get_mut(&key)
            .ok_or_else(|| Error::from_static(ENOENT))?;
        if resolution.prefix != prefix {
            return Err(Error::from_static(ENOENT));
        }
        if resolution.reply.is_some() {
            return Err(Error::from_static(
                "child directory resolution already completed",
            ));
        }
        resolution.reply = Some(reply);
        drop(state);
        self.shared.1.notify_all();
        Ok(())
    }
}
