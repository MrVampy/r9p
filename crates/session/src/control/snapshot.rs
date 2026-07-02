use super::json;
use crate::{is_dir, is_symlink, read_open_directory_entries, Client, Result, OREAD};
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

fn collect_snapshot(
    client: &Client,
    segments: &[Vec<u8>],
    depth: usize,
    timeout: Duration,
    entries: &mut Vec<SnapshotEntry>,
) -> Result<()> {
    let fid = if segments.is_empty() {
        client.clone_fid_timeout(client.root_fid(), timeout)?
    } else {
        client.walk_timeout(client.root_fid(), segments, timeout)?
    };
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
    client.clunk_timeout(fid, timeout)?;
    Ok(())
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
