use std::{
    io::{self, Read, Write},
    sync::mpsc,
    thread,
};

use session::Client;

use crate::errors::{cli_error, CliResult};
use crate::target::{Config, Target};
use crate::{usage, READ_CHUNK};

use super::duplex::open_stream;

pub(crate) fn stream_cmd(config: Config, args: Vec<String>) -> CliResult<()> {
    if args.len() != 1 {
        usage();
    }
    let target = Target {
        config,
        path: args[0].clone(),
    };
    let (client, reader_fid, writer_fid) = open_stream(&target)?;
    let writer_client = client.clone();
    let failure_client = client.clone();
    let (writer_outcome_tx, writer_outcome_rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let failure = copy_stdin(writer_client, writer_fid)
            .err()
            .map(|error| error.to_string());
        let should_shutdown = failure.is_some();
        let _ = writer_outcome_tx.send(failure);
        if should_shutdown {
            let _ = failure_client.shutdown();
        }
    });

    let read_result = copy_stdout(&client, reader_fid);
    let clunk_result = client.clunk(reader_fid);
    let shutdown_result = client.shutdown();

    if let Ok(Some(error)) = writer_outcome_rx.try_recv() {
        return Err(cli_error(format!("stream input failed: {error}")));
    }
    read_result?;
    clunk_result?;
    shutdown_result?;
    Ok(())
}

fn copy_stdin(client: Client, fid: r9p::fid::Fid) -> CliResult<()> {
    let mut stdin = io::stdin().lock();
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stdin.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let written = client.write(fid, offset, &buffer[..count])?;
        if usize::try_from(written).ok() != Some(count) {
            return Err(cli_error("short stream write"));
        }
        offset = offset.saturating_add(u64::from(written));
    }
    client.clunk(fid)?;
    Ok(())
}

fn copy_stdout(client: &Client, fid: r9p::fid::Fid) -> CliResult<()> {
    let mut stdout = io::stdout().lock();
    let mut offset = 0_u64;
    loop {
        let data = client.read(fid, offset, READ_CHUNK)?;
        if data.is_empty() {
            break;
        }
        offset = offset.saturating_add(
            u64::try_from(data.len()).map_err(|_| cli_error("read count overflow"))?,
        );
        stdout.write_all(&data)?;
        stdout.flush()?;
    }
    Ok(())
}
