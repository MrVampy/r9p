use std::{
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use cli::{cli_error, CliResult};

const MINIMUM_KILOBYTES: u64 = 128;
const MAXIMUM_KILOBYTES: u64 = 16 * 1024;
const DEFAULT_ATTEMPTS: usize = 50;
const MAXIMUM_ATTEMPTS: usize = 600;
const RETRY_INTERVAL: Duration = Duration::from_millis(100);

struct Config {
    mountpoint: PathBuf,
    kilobytes: u64,
    attempts: usize,
}

pub(super) fn run(args: Vec<String>) -> CliResult<()> {
    let config = parse(args)?;
    let mut attempt = 0;
    let path = loop {
        match ready_backing_device_read_ahead_path(
            &config.mountpoint,
            Path::new("/proc/self/mountinfo"),
            Path::new("/sys/class/bdi"),
        ) {
            Ok(path) => break path,
            Err(_error) if attempt < config.attempts => {
                attempt += 1;
                thread::sleep(RETRY_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    };
    write_and_verify(&path, config.kilobytes)?;
    println!(
        "mountpoint {} read_ahead_kb {}",
        config.mountpoint.display(),
        config.kilobytes
    );
    Ok(())
}

fn ready_backing_device_read_ahead_path(
    mountpoint: &Path,
    mountinfo_path: &Path,
    bdi_root: &Path,
) -> CliResult<PathBuf> {
    let before = std::fs::read_to_string(mountinfo_path)
        .map_err(|error| cli_error(format!("read mountinfo: {error}")))?;
    let before = backing_device(&before, mountpoint, bdi_root)?;
    require_ready_mountpoint(mountpoint, before.owner_uid, before.owner_gid)?;
    let after = std::fs::read_to_string(mountinfo_path)
        .map_err(|error| cli_error(format!("read mountinfo: {error}")))?;
    let after = backing_device(&after, mountpoint, bdi_root)?;
    if before != after {
        return Err(cli_error("r9p_mount_replaced_during_readiness"));
    }
    Ok(after.read_ahead_path)
}

fn require_ready_mountpoint(mountpoint: &Path, owner_uid: u32, owner_gid: u32) -> CliResult<()> {
    const IDENTITY_UNAVAILABLE: libc::c_int = 120;
    const MOUNT_NOT_READY: libc::c_int = 121;
    const NOT_DIRECTORY: libc::c_int = 122;

    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(cli_error("r9p_mount_owner_probe_fork_failed"));
    }
    if child == 0 {
        let identity_ready = unsafe {
            libc::setresgid(owner_gid, owner_gid, owner_gid) == 0
                && libc::setresuid(owner_uid, owner_uid, owner_uid) == 0
        };
        let status = if !identity_ready {
            IDENTITY_UNAVAILABLE
        } else {
            match std::fs::metadata(mountpoint) {
                Ok(metadata) if metadata.is_dir() => 0,
                Ok(_) => NOT_DIRECTORY,
                Err(_) => MOUNT_NOT_READY,
            }
        };
        unsafe { libc::_exit(status) }
    }

    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(child, &mut status, 0) };
        if waited == child {
            break;
        }
        if waited < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(cli_error("r9p_mount_owner_probe_wait_failed"));
    }
    if !libc::WIFEXITED(status) {
        return Err(cli_error("r9p_mount_owner_probe_terminated"));
    }
    match libc::WEXITSTATUS(status) {
        0 => Ok(()),
        IDENTITY_UNAVAILABLE => Err(cli_error("r9p_mount_owner_identity_unavailable")),
        MOUNT_NOT_READY => Err(cli_error("r9p_mount_not_ready_as_owner")),
        NOT_DIRECTORY => Err(cli_error("r9p_mountpoint_not_directory")),
        _ => Err(cli_error("r9p_mount_owner_probe_invalid_result")),
    }
}

fn parse(args: Vec<String>) -> CliResult<Config> {
    let mut mountpoint = None;
    let mut kilobytes = None;
    let mut attempts = DEFAULT_ATTEMPTS;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--mountpoint" => {
                index += 1;
                mountpoint = Some(PathBuf::from(
                    args.get(index)
                        .ok_or_else(|| cli_error("missing mountpoint"))?,
                ));
            }
            "--kilobytes" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing read-ahead kilobytes"))?;
                kilobytes = Some(
                    value
                        .parse::<u64>()
                        .ok()
                        .filter(|value| (MINIMUM_KILOBYTES..=MAXIMUM_KILOBYTES).contains(value))
                        .ok_or_else(|| cli_error("read-ahead kilobytes outside allowed range"))?,
                );
            }
            "--attempts" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| cli_error("missing read-ahead attempts"))?;
                attempts = value
                    .parse::<usize>()
                    .ok()
                    .filter(|value| *value <= MAXIMUM_ATTEMPTS)
                    .ok_or_else(|| cli_error("read-ahead attempts outside allowed range"))?;
            }
            value => return Err(cli_error(format!("unknown read-ahead option {value}"))),
        }
        index += 1;
    }
    let mountpoint = mountpoint.ok_or_else(|| cli_error("missing --mountpoint"))?;
    if !mountpoint.is_absolute() {
        return Err(cli_error("read-ahead mountpoint must be absolute"));
    }
    Ok(Config {
        mountpoint,
        kilobytes: kilobytes.ok_or_else(|| cli_error("missing --kilobytes"))?,
        attempts,
    })
}

#[derive(Debug, Eq, PartialEq)]
struct BackingDevice {
    read_ahead_path: PathBuf,
    owner_uid: u32,
    owner_gid: u32,
}

fn backing_device(mountinfo: &str, mountpoint: &Path, bdi_root: &Path) -> CliResult<BackingDevice> {
    let target = mountpoint
        .to_str()
        .ok_or_else(|| cli_error("mountpoint is not valid UTF-8"))?;
    let records = mountinfo
        .lines()
        .filter_map(parse_mount_record)
        .filter(|record| record.target == target)
        .collect::<Vec<_>>();
    let record = match records.as_slice() {
        [record] => record,
        [] => return Err(cli_error(format!("r9p_mount_absent:{target}"))),
        _ => {
            return Err(cli_error(format!(
                "r9p_mount_stacked_layers:{target}:{}",
                records.len()
            )))
        }
    };
    if record.filesystem != "fuse.r9p" || !record.source.starts_with("r9p:") {
        return Err(cli_error(format!("r9p_mount_type_invalid:{target}")));
    }
    let owner_uid = record
        .owner_uid
        .ok_or_else(|| cli_error(format!("r9p_mount_owner_missing:{target}")))?;
    let owner_gid = record
        .owner_gid
        .ok_or_else(|| cli_error(format!("r9p_mount_group_missing:{target}")))?;
    Ok(BackingDevice {
        read_ahead_path: bdi_root.join(&record.major_minor).join("read_ahead_kb"),
        owner_uid,
        owner_gid,
    })
}

fn write_and_verify(path: &Path, kilobytes: u64) -> CliResult<()> {
    std::fs::write(path, format!("{kilobytes}\n")).map_err(|error| {
        cli_error(format!(
            "r9p_mount_read_ahead_write_failed:{}:{error}",
            path.display()
        ))
    })?;
    let observed = std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| cli_error("r9p_mount_read_ahead_observation_invalid"))?;
    if observed != kilobytes {
        return Err(cli_error(format!(
            "r9p_mount_read_ahead_mismatch:expected={kilobytes}:observed={observed}"
        )));
    }
    Ok(())
}

struct MountRecord {
    major_minor: String,
    target: String,
    filesystem: String,
    source: String,
    owner_uid: Option<u32>,
    owner_gid: Option<u32>,
}

fn parse_mount_record(line: &str) -> Option<MountRecord> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let separator = fields.iter().position(|field| *field == "-")?;
    if separator < 6 || fields.len() <= separator + 2 {
        return None;
    }
    Some(MountRecord {
        major_minor: fields[2].to_string(),
        target: super::decode_mountinfo_path(fields[4]),
        filesystem: fields[separator + 1].to_string(),
        source: fields[separator + 2].to_string(),
        owner_uid: fields
            .get(separator + 3)
            .and_then(|options| mount_option_u32(options, "user_id")),
        owner_gid: fields
            .get(separator + 3)
            .and_then(|options| mount_option_u32(options, "group_id")),
    })
}

fn mount_option_u32(options: &str, name: &str) -> Option<u32> {
    options.split(',').find_map(|option| {
        let (key, value) = option.split_once('=')?;
        (key == name).then(|| value.parse::<u32>().ok()).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = concat!(
        "68 30 0:85 / /home/mrvamp/Newsgroups rw,nosuid - fuse.r9p r9p:%2Fsources%2Fnewsgroups rw,user_id=1000,group_id=100\n",
        "69 30 8:1 / /mnt/local rw - ext4 /dev/sda1 rw\n",
    );

    #[test]
    fn derives_only_the_exact_r9p_backing_device() {
        let device = backing_device(
            MOUNTINFO,
            Path::new("/home/mrvamp/Newsgroups"),
            Path::new("/sys/class/bdi"),
        )
        .expect("device");
        assert_eq!(
            device.read_ahead_path,
            Path::new("/sys/class/bdi/0:85/read_ahead_kb")
        );
        assert_eq!(device.owner_uid, 1000);
        assert_eq!(device.owner_gid, 100);
        assert!(backing_device(
            MOUNTINFO,
            Path::new("/mnt/local"),
            Path::new("/sys/class/bdi"),
        )
        .is_err());
    }

    #[test]
    fn requires_the_kernel_reported_fuse_owner() {
        let without_owner = MOUNTINFO.replace("user_id=1000,", "");
        let error = backing_device(
            &without_owner,
            Path::new("/home/mrvamp/Newsgroups"),
            Path::new("/sys/class/bdi"),
        )
        .expect_err("FUSE owner must be declared");
        assert!(error.to_string().contains("r9p_mount_owner_missing"));

        let without_group = MOUNTINFO.replace(",group_id=100", "");
        let error = backing_device(
            &without_group,
            Path::new("/home/mrvamp/Newsgroups"),
            Path::new("/sys/class/bdi"),
        )
        .expect_err("FUSE group must be declared");
        assert!(error.to_string().contains("r9p_mount_group_missing"));
    }

    #[test]
    fn rejects_stacked_mount_layers() {
        let stacked = format!("{MOUNTINFO}{}", MOUNTINFO.lines().next().unwrap());
        assert!(backing_device(
            &stacked,
            Path::new("/home/mrvamp/Newsgroups"),
            Path::new("/sys/class/bdi"),
        )
        .is_err());
    }

    #[test]
    fn bounds_the_configured_window() {
        let config = parse(vec![
            "--mountpoint".to_string(),
            "/mnt/data".to_string(),
            "--kilobytes".to_string(),
            "4096".to_string(),
            "--attempts".to_string(),
            "100".to_string(),
        ])
        .expect("config");
        assert_eq!(config.attempts, 100);
        assert!(parse(vec![
            "--mountpoint".to_string(),
            "/mnt/data".to_string(),
            "--kilobytes".to_string(),
            "65536".to_string(),
        ])
        .is_err());
        assert!(parse(vec![
            "--mountpoint".to_string(),
            "/mnt/data".to_string(),
            "--kilobytes".to_string(),
            "4096".to_string(),
            "--attempts".to_string(),
            "601".to_string(),
        ])
        .is_err());
    }

    #[test]
    fn writes_and_verifies_the_observed_window() {
        let directory =
            std::env::temp_dir().join(format!("r9p-read-ahead-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory");
        let path = directory.join("read_ahead_kb");
        std::fs::write(&path, "128\n").expect("initial value");
        write_and_verify(&path, 4096).expect("verified write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "4096\n");
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn readiness_requires_a_served_directory() {
        let directory =
            std::env::temp_dir().join(format!("r9p-ready-mount-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory");
        let owner_uid = unsafe { libc::geteuid() };
        let owner_gid = unsafe { libc::getegid() };
        require_ready_mountpoint(&directory, owner_uid, owner_gid).expect("ready directory");
        let file = directory.join("file");
        std::fs::write(&file, b"file").expect("file");
        assert!(require_ready_mountpoint(&file, owner_uid, owner_gid).is_err());
        std::fs::remove_dir_all(directory).expect("cleanup");
    }
}
