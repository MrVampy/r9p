use crate::{cli_error, CliResult};
use std::{
    env,
    ffi::{OsStr, OsString},
    os::unix::process::CommandExt,
    process::Command,
};

pub(crate) fn dispatch_if_required(arguments: &[OsString]) -> CliResult<()> {
    if !requires_mount_helper(arguments) {
        return Ok(());
    }
    let helper = env::var_os("R9P_MOUNT_HELPER")
        .or_else(|| env::var_os("CARGO_BIN_EXE_r9p-mount"))
        .unwrap_or_else(|| OsString::from("r9p-mount"));
    let error = Command::new(&helper).args(&arguments[1..]).exec();
    Err(cli_error(format!(
        "r9p mount helper {} could not replace the client: {error}",
        helper.to_string_lossy()
    )))
}

pub(crate) fn direct_mount_arguments(arguments: &[OsString]) -> CliResult<Option<Vec<String>>> {
    let Some((command_index, command)) = top_level_command(arguments) else {
        return Ok(None);
    };
    if command != OsStr::new("mount") {
        return Ok(None);
    }
    arguments[command_index + 1..]
        .iter()
        .map(|argument| {
            argument
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| cli_error("r9p mount arguments must be UTF-8"))
        })
        .collect::<CliResult<Vec<_>>>()
        .map(Some)
}

fn requires_mount_helper(arguments: &[OsString]) -> bool {
    let Some((command_index, command)) = top_level_command(arguments) else {
        return false;
    };
    if command == OsStr::new("mount") {
        return true;
    }
    command == OsStr::new("session")
        && arguments
            .get(command_index + 1)
            .is_some_and(|command| command == OsStr::new("serve"))
        && arguments[command_index + 2..]
            .iter()
            .any(|argument| mount_session_option(argument))
}

fn top_level_command(arguments: &[OsString]) -> Option<(usize, &OsStr)> {
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        let text = argument.to_str()?;
        if text == "--" {
            return arguments
                .get(index + 1)
                .map(|command| (index + 1, command.as_os_str()));
        }
        if matches!(text, "-n" | "-D" | "--machine") {
            index += 1;
            continue;
        }
        if global_option_with_inline_value(text) {
            index += 1;
            continue;
        }
        if global_option_with_separate_value(text) {
            index += 2;
            continue;
        }
        if text.starts_with('-') {
            return None;
        }
        return Some((index, argument.as_os_str()));
    }
    None
}

fn global_option_with_inline_value(argument: &str) -> bool {
    [
        "--bind=",
        "--auth-config=",
        "--auth-domain=",
        "--request-timeout=",
        "--control-timeout=",
    ]
    .iter()
    .any(|prefix| argument.starts_with(prefix))
        || ["-a", "-A", "-u", "-m"]
            .iter()
            .any(|prefix| argument.starts_with(prefix) && argument.len() > prefix.len())
}

fn global_option_with_separate_value(argument: &str) -> bool {
    matches!(
        argument,
        "-a" | "--bind"
            | "-A"
            | "-u"
            | "-m"
            | "--auth-config"
            | "--auth-domain"
            | "--request-timeout"
            | "--control-timeout"
    )
}

fn mount_session_option(argument: &OsString) -> bool {
    argument.to_str().is_some_and(|argument| {
        argument == "--mount"
            || argument.starts_with("--mount=")
            || argument.starts_with("--mount-")
    })
}

#[cfg(test)]
mod tests {
    use super::{direct_mount_arguments, requires_mount_helper};
    use std::ffi::OsString;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn direct_mount_dispatches_after_global_options() {
        let arguments = args(&[
            "r9p",
            "--auth-domain",
            "coordinator",
            "-m1048576",
            "mount",
            "--source",
            "/files",
            "m7.mesh:9564",
            "/tmp/files",
        ]);
        assert!(requires_mount_helper(&arguments));
        assert_eq!(
            direct_mount_arguments(&arguments).expect("mount arguments"),
            Some(vec![
                "--source".to_string(),
                "/files".to_string(),
                "m7.mesh:9564".to_string(),
                "/tmp/files".to_string(),
            ])
        );
    }

    #[test]
    fn supervisor_separator_reaches_the_mount_helper_unchanged() {
        let arguments = args(&[
            "r9p",
            "mount",
            "ensure",
            "--mountpoint",
            "/tmp/files",
            "--",
            "--source",
            "/files",
            "m7.mesh:9564",
            "/tmp/files",
        ]);
        assert!(direct_mount_arguments(&arguments)
            .expect("mount arguments")
            .is_some_and(|arguments| arguments.contains(&"--".to_string())));
    }

    #[test]
    fn session_mount_options_dispatch_only_for_session_serve() {
        assert!(requires_mount_helper(&args(&[
            "r9p",
            "session",
            "serve",
            "--socket",
            "/tmp/session.sock",
            "--mount-read-cache",
            "/var/cache/r9p",
            "m7.mesh:9564",
        ])));
        assert!(!requires_mount_helper(&args(&[
            "r9p", "session", "status", "--socket", "--mount",
        ])));
    }

    #[test]
    fn ordinary_paths_and_global_values_named_mount_stay_in_the_client() {
        assert!(!requires_mount_helper(&args(&[
            "r9p",
            "--auth-domain",
            "mount",
            "read",
            "mount",
        ])));
        assert!(!requires_mount_helper(&args(&[
            "r9p", "read", "--", "--mount",
        ])));
    }
}
