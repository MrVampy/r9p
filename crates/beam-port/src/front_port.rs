use crate::{hex, parse_u64};
use front::{serve::ServeHandle, Front, RequestContext};
use std::{collections::HashMap, path::Path, time::Duration};

#[derive(Default)]
pub(crate) struct FrontManager {
    fronts: HashMap<u64, FrontState>,
    next_front_id: u64,
}

struct FrontState {
    front: Front,
    serves: Vec<ServeHandle>,
}

pub(crate) struct PendingRequest {
    front: Front,
    timeout: Duration,
}

impl PendingRequest {
    pub(crate) fn complete(self) -> Result<String, String> {
        let request = self
            .front
            .next_request(self.timeout)
            .map_err(|error| error.to_string())?;
        Ok(match request {
            Some(request) => request_output(&request),
            None => "front-timeout".to_string(),
        })
    }
}

impl Drop for FrontState {
    fn drop(&mut self) {
        for serve in self.serves.drain(..) {
            serve.shutdown();
        }
    }
}

impl FrontManager {
    pub(crate) fn pending_request(
        &self,
        fields: &[&str],
    ) -> Option<Result<PendingRequest, String>> {
        match fields {
            ["front-next-request", raw_id, timeout_ms] => {
                Some(self.front_ref(raw_id).and_then(|state| {
                    parse_u64("timeout_ms", timeout_ms).map(|timeout_ms| PendingRequest {
                        front: state.front.clone(),
                        timeout: Duration::from_millis(timeout_ms),
                    })
                }))
            }
            _ => None,
        }
    }

    pub(crate) fn handle(&mut self, fields: &[&str]) -> Result<String, String> {
        match fields {
            ["front-new"] => {
                let id = self.allocate_id();
                self.fronts.insert(
                    id,
                    FrontState {
                        front: Front::new(),
                        serves: Vec::new(),
                    },
                );
                Ok(format!("front\t{id}"))
            }
            ["front-stop", raw_id] => {
                let id = parse_u64("front_id", raw_id)?;
                self.fronts
                    .remove(&id)
                    .ok_or_else(|| format!("unknown_front:{id}"))?;
                Ok("front-stop".to_string())
            }
            ["front-process-id", raw_id] => {
                let _ = self.front(raw_id)?;
                Ok(format!("front-process-id\t{}", std::process::id()))
            }
            ["front-set-wait-timeout", raw_id, timeout_ms] => {
                let state = self.front(raw_id)?;
                let timeout_ms = parse_u64("timeout_ms", timeout_ms)?;
                state
                    .front
                    .set_wait_timeout(Duration::from_millis(timeout_ms))
                    .map_err(|error| error.to_string())?;
                Ok("front-set-wait-timeout".to_string())
            }
            ["front-set", raw_id, path, data] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                let data = hex::decode(data)?;
                state
                    .front
                    .set(&path, &data)
                    .map_err(|error| error.to_string())?;
                Ok("front-set".to_string())
            }
            ["front-remove-subtree", raw_id, path] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                state
                    .front
                    .remove_subtree_if_exists(&path)
                    .map_err(|error| error.to_string())?;
                Ok("front-remove-subtree".to_string())
            }
            ["front-register-log", raw_id, path] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                state
                    .front
                    .register_log(&path)
                    .map_err(|error| error.to_string())?;
                Ok("front-register-log".to_string())
            }
            ["front-append-event", raw_id, path, data] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                let data = hex::decode(data)?;
                state
                    .front
                    .append_event(&path, &data)
                    .map_err(|error| error.to_string())?;
                Ok("front-append-event".to_string())
            }
            ["front-register-rpc", raw_id, path] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                state
                    .front
                    .register_rpc(&path)
                    .map_err(|error| error.to_string())?;
                Ok("front-register-rpc".to_string())
            }
            ["front-register-read-relay", raw_id, path] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                state
                    .front
                    .register_read_relay(&path)
                    .map_err(|error| error.to_string())?;
                Ok("front-register-read-relay".to_string())
            }
            ["front-register-snapshot-read-relay", raw_id, path] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                state
                    .front
                    .register_snapshot_read_relay(&path)
                    .map_err(|error| error.to_string())?;
                Ok("front-register-snapshot-read-relay".to_string())
            }
            ["front-register-remove-relay", raw_id, path] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                state
                    .front
                    .register_remove_relay(&path)
                    .map_err(|error| error.to_string())?;
                Ok("front-register-remove-relay".to_string())
            }
            ["front-serve-tcp", raw_id, bind] => {
                let state = self.front(raw_id)?;
                let bind = hex::decode_text(bind)?;
                let serve = state
                    .front
                    .serve_tcp(&bind)
                    .map_err(|error| error.to_string())?;
                let addr = serve.addr().to_string();
                state.serves.push(serve);
                Ok(format!("front-serve-tcp\t{}", hex::encode(addr.as_bytes())))
            }
            ["front-serve-tcp-authenticated", raw_id, bind, auth_config_path] => {
                let state = self.front(raw_id)?;
                let bind = hex::decode_text(bind)?;
                let auth_config_path = hex::decode_text(auth_config_path)?;
                let serve = state
                    .front
                    .serve_tcp_authenticated(&bind, Path::new(&auth_config_path))
                    .map_err(|error| error.to_string())?;
                let addr = serve.addr().to_string();
                state.serves.push(serve);
                Ok(format!(
                    "front-serve-tcp-authenticated\t{}",
                    hex::encode(addr.as_bytes())
                ))
            }
            ["front-complete-request", raw_id, prefix, request_id, data] => {
                let state = self.front(raw_id)?;
                let prefix = hex::decode_text(prefix)?;
                let request_id = parse_u64("request_id", request_id)?;
                let data = hex::decode(data)?;
                state
                    .front
                    .complete_request(&prefix, request_id, &data)
                    .map_err(|error| error.to_string())?;
                Ok("front-complete-request".to_string())
            }
            ["front-reject-request", raw_id, prefix, request_id, message] => {
                let state = self.front(raw_id)?;
                let prefix = hex::decode_text(prefix)?;
                let request_id = parse_u64("request_id", request_id)?;
                let message = hex::decode_text(message)?;
                state
                    .front
                    .reject_request(&prefix, request_id, &message)
                    .map_err(|error| error.to_string())?;
                Ok("front-reject-request".to_string())
            }
            ["front-complete-remove", raw_id, prefix, request_id] => {
                let state = self.front(raw_id)?;
                let prefix = hex::decode_text(prefix)?;
                let request_id = parse_u64("request_id", request_id)?;
                state
                    .front
                    .complete_remove(&prefix, request_id)
                    .map_err(|error| error.to_string())?;
                Ok("front-complete-remove".to_string())
            }
            ["front-reject-remove", raw_id, prefix, request_id, message] => {
                let state = self.front(raw_id)?;
                let prefix = hex::decode_text(prefix)?;
                let request_id = parse_u64("request_id", request_id)?;
                let message = hex::decode_text(message)?;
                state
                    .front
                    .reject_remove(&prefix, request_id, &message)
                    .map_err(|error| error.to_string())?;
                Ok("front-reject-remove".to_string())
            }
            _ => Err("invalid_r9p_front_port_request".to_string()),
        }
    }

    fn allocate_id(&mut self) -> u64 {
        self.next_front_id = self.next_front_id.saturating_add(1);
        if self.next_front_id == 0 {
            self.next_front_id = 1;
        }
        self.next_front_id
    }

    fn front(&mut self, raw_id: &str) -> Result<&mut FrontState, String> {
        let id = parse_u64("front_id", raw_id)?;
        self.fronts
            .get_mut(&id)
            .ok_or_else(|| format!("unknown_front:{id}"))
    }

    fn front_ref(&self, raw_id: &str) -> Result<&FrontState, String> {
        let id = parse_u64("front_id", raw_id)?;
        self.fronts
            .get(&id)
            .ok_or_else(|| format!("unknown_front:{id}"))
    }
}

fn request_output(request: &front::IntakeRequest) -> String {
    format!(
        "front-request\t{}\t{}\t{}\t{}",
        hex::encode(request.prefix.as_bytes()),
        request.request_id,
        hex::encode(&request.bytes),
        context_output(&request.context),
    )
}

fn context_output(context: &RequestContext) -> String {
    [
        hex::encode(context.principal_id.as_bytes()),
        hex::encode(context.uname.as_bytes()),
        hex::encode(context.aname.as_bytes()),
        context.session_id.to_string(),
        context.fid.to_string(),
        hex::encode(context.front_path.as_bytes()),
        hex::encode(context.target_path.as_bytes()),
        context.offset.to_string(),
        context.count.to_string(),
        context.open_mode.to_string(),
        context.pushed_generation.to_string(),
    ]
    .join("\t")
}
