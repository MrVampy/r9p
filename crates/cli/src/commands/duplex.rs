use std::time::Duration;

use session::{Client, ConnectionConfig, OREAD, OWRITE};

use crate::errors::{cli_error, CliResult};
use crate::target::{split_namespace_path, target_path, Target};

pub(super) fn open_stream(target: &Target) -> CliResult<(Client, r9p::fid::Fid, r9p::fid::Fid)> {
    let (config, path, timeout) = stream_target(target)?;
    let client = Client::connect_with_timeout(&config, timeout)?;
    let reader_fid = client.walk_path(&path)?;
    client.open(reader_fid, OREAD)?;
    let writer_fid = client.walk_path(&path)?;
    client.open(writer_fid, OWRITE)?;
    Ok((client, reader_fid, writer_fid))
}

pub(super) fn stream_target(target: &Target) -> CliResult<(ConnectionConfig, String, Duration)> {
    let (address, path) = match &target.config.address {
        Some(address) => (address.clone(), target_path(target)?),
        None => {
            if target.config.auth_config.is_some() {
                return Err(cli_error(
                    "--auth-config requires an endpoint supplied with -a or --bind",
                ));
            }
            let (service, path) = split_namespace_path(&target.path)?;
            (format!("namespace!{service}"), path)
        }
    };
    let timeout = target.config.request_timeout.unwrap_or(Duration::ZERO);
    Ok((
        ConnectionConfig {
            address,
            uname: target.config.uname.clone(),
            aname: target.config.aname.clone(),
            msize: target.config.msize,
            authentication: crate::target::client_authentication(&target.config)?,
        },
        path,
        timeout,
    ))
}
