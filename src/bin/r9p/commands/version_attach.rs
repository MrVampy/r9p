use std::io::Write;

use r9p::{
    client::{Client as ProtocolClient, ClientResponse, Completion},
    codec,
    error::Error as R9pError,
};

use crate::commands::machine::print_machine_qid;
use crate::errors::{cli_error, CliResult};
use crate::format::{format_attach, format_version, hex_encode};
use crate::io::connect_path;
use crate::target::{connection_target, Config};
use crate::transport::{dial_target, read_response};

pub(crate) fn version_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if config.machine {
        return machine_version_cmd(config, args);
    }
    let target = connection_target(config, args)?;
    let (client, _) = connect_path(&target)?;
    println!("{}", format_version(client.msize(), client.version()));
    Ok(())
}

pub(crate) fn attach_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    let target = connection_target(config, args)?;
    let (client, _) = connect_path(&target)?;
    if target.config.machine {
        print_machine_qid("attach", client.root_qid());
    } else {
        println!("{}", format_attach(client.root_qid()));
    }
    Ok(())
}

pub(crate) fn machine_version_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    let target = connection_target(config, args)?;
    let mut stream = dial_target(&target)?;
    let mut protocol = ProtocolClient::new();
    let request = protocol.version_request(target.config.msize);
    let frame = codec::encode_tmessage(&request)?;
    stream.write_all(&frame)?;
    let response = read_response(&mut stream)?;

    match protocol.receive(response)? {
        ClientResponse::Completion {
            completion: Completion::Version { msize, version },
            ..
        } => {
            println!("version\t{}\t{msize}", hex_encode(&version));
            Ok(())
        }
        ClientResponse::Error { ename, .. } => Err(R9pError::new(ename).into()),
        other => Err(cli_error(format!("unexpected version response: {other:?}"))),
    }
}
