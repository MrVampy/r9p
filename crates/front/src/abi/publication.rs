use r9p::export_descriptor::{
    AuthBoundary, ExportDescriptor, ExportMode, Protocol, TransportClass,
};
use r9p::srv_publish::{
    maintain_r9p_export, publish_r9p_export, R9pExportMaintenanceConfig, R9pExportPublication,
};
use std::collections::BTreeMap;
use std::ffi::c_char;
use std::time::Duration;

use super::{clear_last_error, optional_str_arg, set_last_error, str_arg, FrontAbi, INVALID, OK};

pub(super) fn stop_publications(abi: &FrontAbi) -> Result<(), String> {
    match abi.publications.lock() {
        Ok(mut publications) => {
            for publication in publications.drain(..) {
                publication.shutdown();
            }
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_publish_r9p_export(
    handle: *mut FrontAbi,
    vault_endpoint_bind: *const c_char,
    vault_endpoint_bind_len: usize,
    vault_uname: *const c_char,
    vault_uname_len: usize,
    vault_aname: *const c_char,
    vault_aname_len: usize,
    service_name: *const c_char,
    service_name_len: usize,
    export_endpoint_bind: *const c_char,
    export_endpoint_bind_len: usize,
    export_uname: *const c_char,
    export_uname_len: usize,
    export_aname: *const c_char,
    export_aname_len: usize,
    exported_root: *const c_char,
    exported_root_len: usize,
    transport_class: *const c_char,
    transport_class_len: usize,
    auth: *const c_char,
    auth_len: usize,
    protocol: *const c_char,
    protocol_len: usize,
    local_root_label: *const c_char,
    local_root_label_len: usize,
    pid: u32,
    msize: u32,
    service_unit: *const c_char,
    service_unit_len: usize,
    host_firewall_admission: *const c_char,
    host_firewall_admission_len: usize,
    namespace_mount_paths: *const c_char,
    namespace_mount_paths_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let publication = match unsafe {
        publication_from_args(PublicationRawArgs {
            vault_endpoint_bind,
            vault_endpoint_bind_len,
            vault_uname,
            vault_uname_len,
            vault_aname,
            vault_aname_len,
            service_name,
            service_name_len,
            export_endpoint_bind,
            export_endpoint_bind_len,
            export_uname,
            export_uname_len,
            export_aname,
            export_aname_len,
            exported_root,
            exported_root_len,
            transport_class,
            transport_class_len,
            auth,
            auth_len,
            protocol,
            protocol_len,
            local_root_label,
            local_root_label_len,
            pid,
            msize,
            service_unit,
            service_unit_len,
            host_firewall_admission,
            host_firewall_admission_len,
            namespace_mount_paths,
            namespace_mount_paths_len,
        })
    } {
        Ok(publication) => publication,
        Err(PublicationArgError::Invalid) => return INVALID,
        Err(PublicationArgError::Build(error)) => return set_last_error(abi, error),
    };
    match publish_r9p_export(&publication) {
        Ok(_) => {
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_maintain_r9p_export(
    handle: *mut FrontAbi,
    vault_endpoint_bind: *const c_char,
    vault_endpoint_bind_len: usize,
    vault_uname: *const c_char,
    vault_uname_len: usize,
    vault_aname: *const c_char,
    vault_aname_len: usize,
    service_name: *const c_char,
    service_name_len: usize,
    export_endpoint_bind: *const c_char,
    export_endpoint_bind_len: usize,
    export_uname: *const c_char,
    export_uname_len: usize,
    export_aname: *const c_char,
    export_aname_len: usize,
    exported_root: *const c_char,
    exported_root_len: usize,
    transport_class: *const c_char,
    transport_class_len: usize,
    auth: *const c_char,
    auth_len: usize,
    protocol: *const c_char,
    protocol_len: usize,
    local_root_label: *const c_char,
    local_root_label_len: usize,
    pid: u32,
    msize: u32,
    retry_interval_ms: u32,
    service_unit: *const c_char,
    service_unit_len: usize,
    host_firewall_admission: *const c_char,
    host_firewall_admission_len: usize,
    namespace_mount_paths: *const c_char,
    namespace_mount_paths_len: usize,
) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    let publication = match unsafe {
        publication_from_args(PublicationRawArgs {
            vault_endpoint_bind,
            vault_endpoint_bind_len,
            vault_uname,
            vault_uname_len,
            vault_aname,
            vault_aname_len,
            service_name,
            service_name_len,
            export_endpoint_bind,
            export_endpoint_bind_len,
            export_uname,
            export_uname_len,
            export_aname,
            export_aname_len,
            exported_root,
            exported_root_len,
            transport_class,
            transport_class_len,
            auth,
            auth_len,
            protocol,
            protocol_len,
            local_root_label,
            local_root_label_len,
            pid,
            msize,
            service_unit,
            service_unit_len,
            host_firewall_admission,
            host_firewall_admission_len,
            namespace_mount_paths,
            namespace_mount_paths_len,
        })
    } {
        Ok(publication) => publication,
        Err(PublicationArgError::Invalid) => return INVALID,
        Err(PublicationArgError::Build(error)) => return set_last_error(abi, error),
    };
    let interval = if retry_interval_ms == 0 {
        R9pExportMaintenanceConfig::default().retry_interval
    } else {
        Duration::from_millis(u64::from(retry_interval_ms))
    };
    let maintainer = match maintain_r9p_export(
        publication,
        R9pExportMaintenanceConfig {
            retry_interval: interval,
        },
    ) {
        Ok(maintainer) => maintainer,
        Err(error) => return set_last_error(abi, error),
    };
    match abi.publications.lock() {
        Ok(mut publications) => {
            publications.push(maintainer);
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn r9p_front_reconcile_r9p_exports(handle: *mut FrontAbi) -> i32 {
    let Some(abi) = (unsafe { handle.as_ref() }) else {
        return INVALID;
    };
    match abi.publications.lock() {
        Ok(publications) => {
            for publication in publications.iter() {
                publication.reconcile_now();
            }
            clear_last_error(abi);
            OK
        }
        Err(error) => set_last_error(abi, error),
    }
}

enum PublicationArgError {
    Invalid,
    Build(r9p::Error),
}

struct PublicationRawArgs {
    vault_endpoint_bind: *const c_char,
    vault_endpoint_bind_len: usize,
    vault_uname: *const c_char,
    vault_uname_len: usize,
    vault_aname: *const c_char,
    vault_aname_len: usize,
    service_name: *const c_char,
    service_name_len: usize,
    export_endpoint_bind: *const c_char,
    export_endpoint_bind_len: usize,
    export_uname: *const c_char,
    export_uname_len: usize,
    export_aname: *const c_char,
    export_aname_len: usize,
    exported_root: *const c_char,
    exported_root_len: usize,
    transport_class: *const c_char,
    transport_class_len: usize,
    auth: *const c_char,
    auth_len: usize,
    protocol: *const c_char,
    protocol_len: usize,
    local_root_label: *const c_char,
    local_root_label_len: usize,
    pid: u32,
    msize: u32,
    service_unit: *const c_char,
    service_unit_len: usize,
    host_firewall_admission: *const c_char,
    host_firewall_admission_len: usize,
    namespace_mount_paths: *const c_char,
    namespace_mount_paths_len: usize,
}

unsafe fn publication_from_args(
    args: PublicationRawArgs,
) -> std::result::Result<R9pExportPublication, PublicationArgError> {
    let (
        Some(vault_endpoint_bind),
        Some(vault_uname),
        Some(vault_aname),
        Some(service_name),
        Some(export_endpoint_bind),
        Some(export_uname),
        Some(export_aname),
        Some(exported_root),
        Some(transport_class),
        Some(auth),
        Some(protocol),
        Some(local_root_label),
        Some(service_unit),
        Some(host_firewall_admission),
        Some(namespace_mount_paths),
    ) = (
        unsafe { str_arg(args.vault_endpoint_bind, args.vault_endpoint_bind_len) },
        unsafe { str_arg(args.vault_uname, args.vault_uname_len) },
        unsafe { str_arg(args.vault_aname, args.vault_aname_len) },
        unsafe { str_arg(args.service_name, args.service_name_len) },
        unsafe { str_arg(args.export_endpoint_bind, args.export_endpoint_bind_len) },
        unsafe { str_arg(args.export_uname, args.export_uname_len) },
        unsafe { str_arg(args.export_aname, args.export_aname_len) },
        unsafe { str_arg(args.exported_root, args.exported_root_len) },
        unsafe { str_arg(args.transport_class, args.transport_class_len) },
        unsafe { str_arg(args.auth, args.auth_len) },
        unsafe { str_arg(args.protocol, args.protocol_len) },
        unsafe { optional_str_arg(args.local_root_label, args.local_root_label_len) },
        unsafe { optional_str_arg(args.service_unit, args.service_unit_len) },
        unsafe {
            optional_str_arg(
                args.host_firewall_admission,
                args.host_firewall_admission_len,
            )
        },
        unsafe { optional_str_arg(args.namespace_mount_paths, args.namespace_mount_paths_len) },
    )
    else {
        return Err(PublicationArgError::Invalid);
    };
    let transport_class =
        TransportClass::parse(transport_class).map_err(PublicationArgError::Build)?;
    let auth = AuthBoundary::parse(auth).map_err(PublicationArgError::Build)?;
    let protocol = Protocol::parse(protocol).map_err(PublicationArgError::Build)?;
    let mut extra_fields = BTreeMap::new();
    match (service_unit, host_firewall_admission) {
        (Some(service_unit), host_firewall_admission) => {
            extra_fields.insert("service_unit".to_string(), service_unit.to_string());
            extra_fields.insert(
                "host_firewall_admission".to_string(),
                host_firewall_admission
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        derive_host_firewall_admission(transport_class, export_endpoint_bind)
                    }),
            );
        }
        (None, None) => {}
        (None, Some(_)) => {
            return Err(PublicationArgError::Build(r9p::Error::from(
                "host_firewall_admission requires service_unit",
            )));
        }
    }
    Ok(R9pExportPublication {
        vault_endpoint_bind: vault_endpoint_bind.to_string(),
        vault_uname: vault_uname.to_string(),
        vault_aname: vault_aname.to_string(),
        service_name: service_name.to_string(),
        descriptor: ExportDescriptor {
            endpoint_bind: export_endpoint_bind.to_string(),
            aname: export_aname.to_string(),
            uname: export_uname.to_string(),
            exported_root: exported_root.to_string(),
            transport_class,
            mode: ExportMode::ReadOnly,
            auth,
            pid: args.pid,
            protocol,
            msize: args.msize,
            expires_at: None,
            local_root_label: local_root_label.map(str::to_string),
            namespace_mount_paths: namespace_mount_paths_arg(namespace_mount_paths),
            extra_fields,
        },
    })
}

fn namespace_mount_paths_arg(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or("")
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
