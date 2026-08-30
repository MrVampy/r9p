mod direct;
mod session_mount;

use cli::{CliResult, ClientConfig, MountAdapter};
use session::control::{ControlConfig, ControlRuntime};
use std::{ffi::OsString, thread::JoinHandle};

struct MountRuntime {
    session: session_mount::SessionMountConfig,
}

impl MountAdapter for MountRuntime {
    fn direct_mount(&self, config: ClientConfig, args: Vec<String>) -> CliResult<()> {
        direct::mount_cmd(config, args)
    }

    fn start_session_mount(
        &self,
        control: &ControlConfig,
        runtime: &ControlRuntime,
    ) -> CliResult<Option<JoinHandle<()>>> {
        session_mount::start_session_mount(control, runtime, &self.session)
    }
}

fn main() {
    match prepare() {
        Ok((arguments, mount)) => cli::mount_helper_main(arguments, &mount),
        Err(error) => {
            eprintln!("r9p: {error}");
            std::process::exit(1);
        }
    }
}

fn prepare() -> CliResult<(Vec<OsString>, MountRuntime)> {
    let mut arguments = std::env::args_os()
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| cli::cli_error("r9p mount arguments must be UTF-8"))
        })
        .collect::<CliResult<Vec<_>>>()?;
    let session = session_mount::take_session_mount_config(&mut arguments)?;
    Ok((
        arguments.into_iter().map(OsString::from).collect(),
        MountRuntime { session },
    ))
}
