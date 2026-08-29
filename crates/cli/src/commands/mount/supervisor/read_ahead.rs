use std::{
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use crate::errors::{cli_error, CliResult};

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
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|error| cli_error(format!("read mountinfo: {error}")))?;
        match backing_device_read_ahead_path(
            &mountinfo,
            &config.mountpoint,
            Path::new("/sys/class/bdi"),
        ) {
            Ok(path) => break path,
            Err(error) if attempt < config.attempts => {
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

fn backing_device_read_ahead_path(
    mountinfo: &str,
    mountpoint: &Path,
    bdi_root: &Path,
) -> CliResult<PathBuf> {
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
    Ok(bdi_root.join(&record.major_minor).join("read_ahead_kb"))
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &str = concat!(
        "68 30 0:85 / /home/mrvamp/Newsgroups rw,nosuid - fuse.r9p r9p:%2Fsources%2Fnewsgroups rw,user_id=1000\n",
        "69 30 8:1 / /mnt/local rw - ext4 /dev/sda1 rw\n",
    );

    #[test]
    fn derives_only_the_exact_r9p_backing_device() {
        let path = backing_device_read_ahead_path(
            MOUNTINFO,
            Path::new("/home/mrvamp/Newsgroups"),
            Path::new("/sys/class/bdi"),
        )
        .expect("path");
        assert_eq!(path, Path::new("/sys/class/bdi/0:85/read_ahead_kb"));
        assert!(backing_device_read_ahead_path(
            MOUNTINFO,
            Path::new("/mnt/local"),
            Path::new("/sys/class/bdi"),
        )
        .is_err());
    }

    #[test]
    fn rejects_stacked_mount_layers() {
        let stacked = format!("{MOUNTINFO}{}", MOUNTINFO.lines().next().unwrap());
        assert!(backing_device_read_ahead_path(
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
}
