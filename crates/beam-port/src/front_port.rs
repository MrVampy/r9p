use crate::{hex, parse_u32, parse_u64};
use front::{serve::ServeHandle, Front, RequestContext};
use r9p::{
    export_descriptor::{AuthBoundary, ExportDescriptor, ExportMode, Protocol, TransportClass},
    srv_publish::{
        maintain_r9p_export, PublishOutcome, R9pExportMaintainer, R9pExportMaintenanceConfig,
        R9pExportPublication,
    },
};
use std::{collections::BTreeMap, collections::HashMap, time::Duration};

#[derive(Default)]
pub(crate) struct FrontManager {
    fronts: HashMap<u64, FrontState>,
    next_front_id: u64,
}

struct FrontState {
    front: Front,
    serves: Vec<ServeHandle>,
    maintainers: Vec<R9pExportMaintainer>,
}

impl Drop for FrontState {
    fn drop(&mut self) {
        for maintainer in self.maintainers.drain(..) {
            maintainer.shutdown();
        }
        for serve in self.serves.drain(..) {
            serve.shutdown();
        }
    }
}

impl FrontManager {
    pub(crate) fn handle(&mut self, fields: &[&str]) -> Result<String, String> {
        match fields {
            ["front-new"] => {
                let id = self.allocate_id();
                self.fronts.insert(
                    id,
                    FrontState {
                        front: Front::new(),
                        serves: Vec::new(),
                        maintainers: Vec::new(),
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
            ["front-register-rpc", raw_id, path] => {
                let state = self.front(raw_id)?;
                let path = hex::decode_text(path)?;
                state
                    .front
                    .register_rpc(&path)
                    .map_err(|error| error.to_string())?;
                Ok("front-register-rpc".to_string())
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
            ["front-next-request", raw_id, timeout_ms] => {
                let state = self.front(raw_id)?;
                let timeout_ms = parse_u64("timeout_ms", timeout_ms)?;
                let request = state
                    .front
                    .next_request(Duration::from_millis(timeout_ms))
                    .map_err(|error| error.to_string())?;
                Ok(match request {
                    Some(request) => request_output(&request),
                    None => "front-timeout".to_string(),
                })
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
            ["front-maintain-r9p-export", raw_id, vault_endpoint_bind, vault_uname, vault_aname, service_name, export_endpoint_bind, export_uname, export_aname, exported_root, transport_class, auth, protocol, local_root_label, msize, retry_interval_ms, service_unit, host_firewall_admission, namespace_mount_paths] =>
            {
                let state = self.front(raw_id)?;
                let publication = publication_from_fields(PublicationFields {
                    vault_endpoint_bind,
                    vault_uname,
                    vault_aname,
                    service_name,
                    export_endpoint_bind,
                    export_uname,
                    export_aname,
                    exported_root,
                    transport_class,
                    auth,
                    protocol,
                    local_root_label,
                    msize,
                    service_unit,
                    host_firewall_admission,
                    namespace_mount_paths,
                })?;
                let retry_interval_ms = parse_u32("retry_interval_ms", retry_interval_ms)?;
                let retry_interval = if retry_interval_ms == 0 {
                    R9pExportMaintenanceConfig::default().retry_interval
                } else {
                    Duration::from_millis(u64::from(retry_interval_ms))
                };
                let maintainer =
                    maintain_r9p_export(publication, R9pExportMaintenanceConfig { retry_interval })
                        .map_err(|error| error.to_string())?;
                state.maintainers.push(maintainer);
                Ok("front-maintain-r9p-export".to_string())
            }
            ["front-reconcile-r9p-exports", raw_id] => {
                let state = self.front(raw_id)?;
                for maintainer in &state.maintainers {
                    maintainer.reconcile_now();
                }
                Ok("front-reconcile-r9p-exports".to_string())
            }
            ["front-maintenance-status", raw_id] => {
                let state = self.front(raw_id)?;
                Ok(maintenance_status_output(&state.maintainers))
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
}

struct PublicationFields<'a> {
    vault_endpoint_bind: &'a str,
    vault_uname: &'a str,
    vault_aname: &'a str,
    service_name: &'a str,
    export_endpoint_bind: &'a str,
    export_uname: &'a str,
    export_aname: &'a str,
    exported_root: &'a str,
    transport_class: &'a str,
    auth: &'a str,
    protocol: &'a str,
    local_root_label: &'a str,
    msize: &'a str,
    service_unit: &'a str,
    host_firewall_admission: &'a str,
    namespace_mount_paths: &'a str,
}

fn publication_from_fields(fields: PublicationFields<'_>) -> Result<R9pExportPublication, String> {
    let vault_endpoint_bind = hex::decode_text(fields.vault_endpoint_bind)?;
    let vault_uname = hex::decode_text(fields.vault_uname)?;
    let vault_aname = hex::decode_text(fields.vault_aname)?;
    let service_name = hex::decode_text(fields.service_name)?;
    let export_endpoint_bind = hex::decode_text(fields.export_endpoint_bind)?;
    let export_uname = hex::decode_text(fields.export_uname)?;
    let export_aname = hex::decode_text(fields.export_aname)?;
    let exported_root = hex::decode_text(fields.exported_root)?;
    let transport_class = TransportClass::parse(&hex::decode_text(fields.transport_class)?)
        .map_err(|error| error.to_string())?;
    let auth =
        AuthBoundary::parse(&hex::decode_text(fields.auth)?).map_err(|error| error.to_string())?;
    let protocol =
        Protocol::parse(&hex::decode_text(fields.protocol)?).map_err(|error| error.to_string())?;
    let local_root_label = empty_as_none(hex::decode_text(fields.local_root_label)?);
    let msize = parse_u32("msize", fields.msize)?;
    let service_unit = empty_as_none(hex::decode_text(fields.service_unit)?);
    let host_firewall_admission = empty_as_none(hex::decode_text(fields.host_firewall_admission)?);
    let namespace_mount_paths =
        namespace_mount_paths(hex::decode_text(fields.namespace_mount_paths)?);

    let mut extra_fields = BTreeMap::new();
    match (service_unit, host_firewall_admission) {
        (Some(service_unit), host_firewall_admission) => {
            extra_fields.insert("service_unit".to_string(), service_unit);
            extra_fields.insert(
                "host_firewall_admission".to_string(),
                host_firewall_admission.unwrap_or_else(|| {
                    derive_host_firewall_admission(transport_class, &export_endpoint_bind)
                }),
            );
        }
        (None, None) => {}
        (None, Some(_)) => {
            return Err("host_firewall_admission requires service_unit".to_string());
        }
    }

    Ok(R9pExportPublication {
        vault_endpoint_bind,
        vault_uname,
        vault_aname,
        service_name,
        descriptor: ExportDescriptor {
            endpoint_bind: export_endpoint_bind,
            aname: export_aname,
            uname: export_uname,
            exported_root,
            transport_class,
            mode: ExportMode::ReadOnly,
            auth,
            pid: std::process::id(),
            protocol,
            msize,
            expires_at: None,
            local_root_label,
            namespace_mount_paths,
            extra_fields,
        },
    })
}

fn empty_as_none(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn namespace_mount_paths(value: String) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

fn derive_host_firewall_admission(
    transport_class: TransportClass,
    export_endpoint_bind: &str,
) -> String {
    match transport_class {
        TransportClass::Tcp => format!("tcp:{export_endpoint_bind}"),
        TransportClass::Unix => format!("unix:{export_endpoint_bind}"),
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
        context.open_mode.to_string(),
        context.pushed_generation.to_string(),
    ]
    .join("\t")
}

fn maintenance_status_output(maintainers: &[R9pExportMaintainer]) -> String {
    let mut success_count = 0;
    let mut failure_count = 0;
    let mut last_success = None;
    let mut last_error = None;
    for maintainer in maintainers {
        let status = maintainer.status();
        success_count += status.success_count;
        failure_count += status.failure_count;
        if status.last_success.is_some() {
            last_success = status.last_success;
        }
        if status.last_error.is_some() {
            last_error = status.last_error;
        }
    }
    format!(
        "front-maintenance-status\t{success_count}\t{failure_count}\t{}\t{}",
        hex::encode(publish_outcome_option(last_success).as_bytes()),
        hex::encode(last_error.unwrap_or_default().as_bytes()),
    )
}

fn publish_outcome_option(outcome: Option<PublishOutcome>) -> &'static str {
    match outcome {
        Some(PublishOutcome::AlreadyReady) => "already-ready",
        Some(PublishOutcome::Registered) => "registered",
        Some(PublishOutcome::Updated) => "updated",
        None => "",
    }
}
