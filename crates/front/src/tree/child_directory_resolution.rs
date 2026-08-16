use super::*;
use crate::model::{ChildDirectoryReply, ChildDirectoryResolution};
use std::time::Instant;

const MAX_PENDING_RESOLUTIONS: usize = 256;

impl FrontTree {
    pub(super) fn resolve_child_directory(
        &self,
        binding: &FidBinding,
        fid: Fid,
        parent: u64,
        name: &[u8],
    ) -> Result<Option<u64>> {
        let key = (parent, name.to_vec());
        let mut state = self.front.lock()?;
        let resolver = match &state.node(parent)?.body {
            Body::Dir(directory) => match directory.children.get(name).copied() {
                Some(child) => return Ok(Some(child)),
                None => directory.child_resolver.clone(),
            },
            _ => return Ok(None),
        };
        let Some(resolver) = resolver else {
            return Ok(None);
        };

        let mut notify = false;
        if let Some(resolution) = state.child_directory_resolutions.get_mut(&key) {
            resolution.waiters = resolution.waiters.saturating_add(1);
        } else {
            if state.child_directory_resolutions.len() >= MAX_PENDING_RESOLUTIONS {
                return Err(Error::from_static(
                    "child directory resolution capacity reached",
                ));
            }
            let name_text = std::str::from_utf8(name)
                .map_err(|_| Error::from_static("child directory name must be UTF-8"))?;
            let target_path = append_path(state.path_relative_to(parent, binding.root)?, name_text);
            let front_path = append_path(state.path_relative_to(parent, ROOT_ID)?, name_text);
            let context = request_context(
                binding,
                fid,
                front_path,
                target_path,
                RequestDetails {
                    offset: 0,
                    count: 0,
                    open_mode: 0,
                    pushed_generation: state.node(parent)?.generation,
                },
            );
            let request_id = state.next_request_id;
            state.next_request_id = state.next_request_id.saturating_add(1);
            state.child_directory_resolutions.insert(
                key.clone(),
                ChildDirectoryResolution {
                    request_id,
                    prefix: resolver.resolution_prefix.clone(),
                    read_prefix: resolver.read_prefix,
                    reply: None,
                    waiters: 1,
                },
            );
            state
                .child_directory_resolution_requests
                .insert(request_id, key.clone());
            state.pending.push_back(IntakeRequest {
                request_id,
                prefix: resolver.resolution_prefix,
                bytes: name.to_vec(),
                context,
            });
            notify = true;
        }
        let deadline = Instant::now() + state.wait_timeout;
        if notify {
            drop(state);
            self.front.shared.1.notify_all();
            state = self.front.lock()?;
        }

        loop {
            let child = match state.nodes.get(&parent).map(|node| &node.body) {
                Some(Body::Dir(directory)) => directory.children.get(name).copied(),
                Some(_) => {
                    state.finish_child_directory_waiter(&key);
                    return Err(Error::from_static(ENOTDIR));
                }
                None => {
                    state.finish_child_directory_waiter(&key);
                    return Err(Error::from_static(ENOENT));
                }
            };
            if let Some(child) = child {
                state.finish_child_directory_waiter(&key);
                return Ok(Some(child));
            }
            let resolution = state
                .child_directory_resolutions
                .get(&key)
                .ok_or_else(|| Error::from_static(ENOENT))?;
            let reply = resolution.reply.clone();
            let read_prefix = resolution.read_prefix.clone();
            match reply {
                Some(ChildDirectoryReply::Accepted(metadata)) => {
                    match state.insert_pushed_child_directory(parent, name, metadata, read_prefix) {
                        Ok(child) => {
                            state.finish_child_directory_waiter(&key);
                            drop(state);
                            self.front.shared.1.notify_all();
                            return Ok(Some(child));
                        }
                        Err(error) => {
                            if let Some(resolution) =
                                state.child_directory_resolutions.get_mut(&key)
                            {
                                resolution.reply =
                                    Some(ChildDirectoryReply::Rejected(error.to_string()));
                            }
                        }
                    }
                }
                Some(ChildDirectoryReply::Rejected(message)) => {
                    state.finish_child_directory_waiter(&key);
                    return Err(Error::from(message));
                }
                None => {}
            }
            let now = Instant::now();
            if now >= deadline {
                state.finish_child_directory_waiter(&key);
                return Err(Error::from_static(
                    "child directory resolution timed out awaiting response",
                ));
            }
            let (next, _timeout_result) =
                self.front
                    .shared
                    .1
                    .wait_timeout(state, deadline - now)
                    .map_err(|_| Error::from_static("front state poisoned"))?;
            state = next;
        }
    }
}

fn append_path(parent: String, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}
