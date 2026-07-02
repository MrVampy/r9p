use super::json;
use crate::{is_dir, is_symlink, read_open_directory_entries, Client, Result, OREAD};
use r9p::blocking::DEFAULT_READ_CHUNK;
use r9p::stat::Stat;
use std::time::Duration;

pub fn snapshot_json(
    client: &Client,
    path: &str,
    depth: usize,
    timeout: Duration,
) -> Result<String> {
    let segments = parse_namespace_path(path);
    let mut entries = Vec::new();
    collect_snapshot(client, &segments, depth, timeout, &mut entries)?;

    let mut out = String::from("{\"ok\":true,\"kind\":\"session.snapshot.v1\",\"path\":");
    json::push_string(&mut out, &format_path(&segments));
    out.push_str(",\"depth\":");
    out.push_str(&depth.to_string());
    out.push_str(",\"freshness\":{\"state\":\"fresh\"},\"entries\":[");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_snapshot_entry(&mut out, entry);
    }
    out.push_str("]}");
    Ok(out)
}

pub fn stat_json(client: &Client, path: &str, timeout: Duration) -> Result<String> {
    let segments = parse_namespace_path(path);
    with_owned_fid(client, &segments, timeout, |fid| {
        let stat = client.stat_timeout(fid, timeout)?;
        let mut out = String::from("{\"ok\":true,\"kind\":\"session.stat.v1\",\"entry\":");
        push_snapshot_entry(
            &mut out,
            &SnapshotEntry::from_stat(format_path(&segments), &stat),
        );
        out.push('}');
        Ok(out)
    })
}

pub fn list_json(client: &Client, path: &str, timeout: Duration) -> Result<String> {
    let segments = parse_namespace_path(path);
    with_owned_fid(client, &segments, timeout, |fid| {
        let stat = client.stat_timeout(fid, timeout)?;
        if !is_dir(&stat) {
            return Err(crate::Error::new(
                libc::ENOTDIR,
                "session list target is not a directory",
            ));
        }
        client.open_timeout(fid, OREAD, timeout)?;
        let entries = read_open_directory_entries(client, fid, timeout)?;

        let mut out = String::from("{\"ok\":true,\"kind\":\"session.list.v1\",\"path\":");
        json::push_string(&mut out, &format_path(&segments));
        out.push_str(",\"freshness\":{\"state\":\"fresh\"},\"entries\":[");
        for (index, entry) in entries.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let mut child_path = segments.clone();
            child_path.push(entry.name.clone());
            push_snapshot_entry(
                &mut out,
                &SnapshotEntry::from_stat(format_path(&child_path), &entry.stat),
            );
        }
        out.push_str("]}");
        Ok(out)
    })
}

pub fn read_json(client: &Client, path: &str, timeout: Duration) -> Result<String> {
    let segments = parse_namespace_path(path);
    with_owned_fid(client, &segments, timeout, |fid| {
        let stat = client.stat_timeout(fid, timeout)?;
        if is_dir(&stat) {
            return Err(crate::Error::new(
                libc::EISDIR,
                "session read target is a directory",
            ));
        }
        client.open_timeout(fid, OREAD, timeout)?;
        let data = read_all(client, fid, timeout)?;
        let mut out = String::from("{\"ok\":true,\"kind\":\"session.read.v1\",\"path\":");
        json::push_string(&mut out, &format_path(&segments));
        out.push_str(",\"bytes\":");
        out.push_str(&data.len().to_string());
        out.push_str(",\"data_hex\":\"");
        json::push_hex(&mut out, &data);
        out.push_str("\"}");
        Ok(out)
    })
}

fn collect_snapshot(
    client: &Client,
    segments: &[Vec<u8>],
    depth: usize,
    timeout: Duration,
    entries: &mut Vec<SnapshotEntry>,
) -> Result<()> {
    with_owned_fid(client, segments, timeout, |fid| {
        let stat = client.stat_timeout(fid, timeout)?;
        let path = format_path(segments);
        let is_directory = is_dir(&stat);
        entries.push(SnapshotEntry {
            path,
            name: json::bytes_lossy(&stat.name),
            kind: kind(&stat),
            qid_path: stat.qid.path,
            qid_version: stat.qid.version,
            qid_type: stat.qid.qtype,
            mode: stat.mode,
            length: stat.length,
        });

        if is_directory && depth > 0 {
            client.open_timeout(fid, OREAD, timeout)?;
            let children = read_open_directory_entries(client, fid, timeout)?;
            for child in children {
                if child.name == b"." || child.name == b".." {
                    continue;
                }
                let mut child_path = segments.to_vec();
                child_path.push(child.name);
                collect_snapshot(client, &child_path, depth - 1, timeout, entries)?;
            }
        }
        Ok(())
    })
}

fn walk_owned_fid(
    client: &Client,
    segments: &[Vec<u8>],
    timeout: Duration,
) -> Result<r9p::fid::Fid> {
    if segments.is_empty() {
        client.clone_fid_timeout(client.root_fid(), timeout)
    } else {
        client.walk_timeout(client.root_fid(), segments, timeout)
    }
}

fn with_owned_fid<T>(
    client: &Client,
    segments: &[Vec<u8>],
    timeout: Duration,
    body: impl FnOnce(r9p::fid::Fid) -> Result<T>,
) -> Result<T> {
    let fid = walk_owned_fid(client, segments, timeout)?;
    let result = body(fid);
    let clunk = client.clunk_timeout(fid, timeout);
    match (result, clunk) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn read_all(client: &Client, fid: r9p::fid::Fid, timeout: Duration) -> Result<Vec<u8>> {
    let mut offset = 0_u64;
    let mut data = Vec::new();
    loop {
        let chunk = client.read_timeout(fid, offset, DEFAULT_READ_CHUNK, timeout)?;
        if chunk.is_empty() {
            break;
        }
        offset = offset.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        data.extend(chunk);
    }
    Ok(data)
}

pub fn parse_namespace_path(path: &str) -> Vec<Vec<u8>> {
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.as_bytes().to_vec())
        .collect()
}

fn format_path(segments: &[Vec<u8>]) -> String {
    if segments.is_empty() {
        return "/".to_string();
    }
    let mut out = String::new();
    for segment in segments {
        out.push('/');
        out.push_str(&json::bytes_lossy(segment));
    }
    out
}

fn kind(stat: &Stat) -> &'static str {
    if is_dir(stat) {
        "dir"
    } else if is_symlink(stat) {
        "symlink"
    } else {
        "file"
    }
}

fn push_snapshot_entry(out: &mut String, entry: &SnapshotEntry) {
    out.push_str("{\"path\":");
    json::push_string(out, &entry.path);
    out.push_str(",\"name\":");
    json::push_string(out, &entry.name);
    out.push_str(",\"kind\":");
    json::push_string(out, entry.kind);
    out.push_str(",\"qid\":{\"path\":");
    out.push_str(&entry.qid_path.to_string());
    out.push_str(",\"version\":");
    out.push_str(&entry.qid_version.to_string());
    out.push_str(",\"type\":");
    out.push_str(&entry.qid_type.to_string());
    out.push_str("},\"mode\":");
    out.push_str(&entry.mode.to_string());
    out.push_str(",\"length\":");
    out.push_str(&entry.length.to_string());
    out.push('}');
}

struct SnapshotEntry {
    path: String,
    name: String,
    kind: &'static str,
    qid_path: u64,
    qid_version: u32,
    qid_type: u8,
    mode: u32,
    length: u64,
}

impl SnapshotEntry {
    fn from_stat(path: String, stat: &Stat) -> Self {
        Self {
            path,
            name: json::bytes_lossy(&stat.name),
            kind: kind(stat),
            qid_path: stat.qid.path,
            qid_version: stat.qid.version,
            qid_type: stat.qid.qtype,
            mode: stat.mode,
            length: stat.length,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_namespace_path;

    #[test]
    fn parses_absolute_namespace_path_segments() {
        assert_eq!(
            parse_namespace_path("/srv/data"),
            vec![b"srv".to_vec(), b"data".to_vec()]
        );
        assert!(parse_namespace_path("/").is_empty());
    }
}
