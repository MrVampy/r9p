pub(super) use super::snapshot_path::parse_namespace_path;
use super::{
    freshness,
    freshness::ResponseFreshness,
    json,
    options::SnapshotOptions,
    snapshot_path::format_path,
    snapshot_report::{
        push_cache_report, push_degraded_branch, push_snapshot_entry, DegradedBranch,
        SnapshotCacheReport, SnapshotEntry, SnapshotReport,
    },
};
use crate::{is_dir, read_open_directory_entries, Client, DirEntry, NamespaceCache, Result, OREAD};
use r9p::blocking::DEFAULT_READ_CHUNK;
use r9p::stat::Stat;
use std::time::Duration;

pub(super) struct SnapshotRequest<'a> {
    pub path: &'a str,
    pub depth: usize,
    pub timeout: Duration,
    pub options: &'a SnapshotOptions,
    pub cache_reads_enabled: bool,
    pub response_freshness: &'a ResponseFreshness,
}

struct SnapshotWalk<'a> {
    client: &'a Client,
    cache: &'a NamespaceCache,
    timeout: Duration,
    options: &'a SnapshotOptions,
    cache_reads_enabled: bool,
}

pub fn snapshot_json(
    client: &Client,
    cache: &NamespaceCache,
    path: &str,
    depth: usize,
    timeout: Duration,
    cache_reads_enabled: bool,
    response_freshness: &ResponseFreshness,
) -> Result<String> {
    let options = SnapshotOptions::default();
    snapshot_json_with_options(
        client,
        cache,
        SnapshotRequest {
            path,
            depth,
            timeout,
            options: &options,
            cache_reads_enabled,
            response_freshness,
        },
    )
}

pub(super) fn snapshot_json_with_options(
    client: &Client,
    cache: &NamespaceCache,
    request: SnapshotRequest<'_>,
) -> Result<String> {
    let segments = parse_namespace_path(request.path);
    let mut report = SnapshotReport::default();
    let walk = SnapshotWalk {
        client,
        cache,
        timeout: request.timeout,
        options: request.options,
        cache_reads_enabled: request.cache_reads_enabled,
    };
    collect_snapshot(&walk, &segments, request.depth, &mut report)?;

    let mut out = String::from("{\"ok\":true,\"kind\":\"session.snapshot.v1\",\"path\":");
    json::push_string(&mut out, &format_path(&segments));
    out.push_str(",\"depth\":");
    out.push_str(&request.depth.to_string());
    out.push_str(",\"freshness\":");
    freshness::push_json(&mut out, request.response_freshness);
    out.push_str(",\"cache\":");
    push_cache_report(&mut out, &report.cache, request.cache_reads_enabled);
    out.push_str(",\"entries\":[");
    for (index, entry) in report.entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_snapshot_entry(&mut out, entry, request.options);
    }
    out.push_str("],\"degraded\":[");
    for (index, degraded) in report.degraded.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        push_degraded_branch(&mut out, degraded);
    }
    out.push_str("]}");
    Ok(out)
}

pub fn stat_json(
    client: &Client,
    cache: &NamespaceCache,
    path: &str,
    timeout: Duration,
    cache_reads_enabled: bool,
    response_freshness: &ResponseFreshness,
) -> Result<String> {
    let segments = parse_namespace_path(path);
    let mut cache_report = SnapshotCacheReport::default();
    let stat = stat_for_path(
        client,
        cache,
        &segments,
        timeout,
        cache_reads_enabled,
        &mut cache_report,
    )?;
    let mut out = String::from("{\"ok\":true,\"kind\":\"session.stat.v1\",\"entry\":");
    push_snapshot_entry(
        &mut out,
        &SnapshotEntry::from_stat(format_path(&segments), &stat),
        &SnapshotOptions::default(),
    );
    out.push_str(",\"freshness\":");
    freshness::push_json(&mut out, response_freshness);
    out.push_str(",\"cache\":");
    push_cache_report(&mut out, &cache_report, cache_reads_enabled);
    out.push('}');
    Ok(out)
}

pub fn list_json(
    client: &Client,
    cache: &NamespaceCache,
    path: &str,
    timeout: Duration,
    cache_reads_enabled: bool,
    response_freshness: &ResponseFreshness,
) -> Result<String> {
    let segments = parse_namespace_path(path);
    let mut cache_report = SnapshotCacheReport::default();
    let stat = stat_for_path(
        client,
        cache,
        &segments,
        timeout,
        cache_reads_enabled,
        &mut cache_report,
    )?;
    if !is_dir(&stat) {
        return Err(crate::Error::new(
            libc::ENOTDIR,
            "session list target is not a directory",
        ));
    }
    let entries = directory_entries_for_path(
        client,
        cache,
        &segments,
        timeout,
        cache_reads_enabled,
        &mut cache_report,
    )?;
    let mut out = String::from("{\"ok\":true,\"kind\":\"session.list.v1\",\"path\":");
    json::push_string(&mut out, &format_path(&segments));
    out.push_str(",\"freshness\":");
    freshness::push_json(&mut out, response_freshness);
    out.push_str(",\"cache\":");
    push_cache_report(&mut out, &cache_report, cache_reads_enabled);
    out.push_str(",\"entries\":[");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let mut child_path = segments.clone();
        child_path.push(entry.name.clone());
        push_snapshot_entry(
            &mut out,
            &SnapshotEntry::from_stat(format_path(&child_path), &entry.stat),
            &SnapshotOptions::default(),
        );
    }
    out.push_str("]}");
    Ok(out)
}

pub fn read_json(
    client: &Client,
    path: &str,
    timeout: Duration,
    response_freshness: &ResponseFreshness,
) -> Result<String> {
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
        out.push_str("\",\"freshness\":");
        freshness::push_json(&mut out, response_freshness);
        out.push('}');
        Ok(out)
    })
}

fn collect_snapshot(
    walk: &SnapshotWalk<'_>,
    segments: &[Vec<u8>],
    depth: usize,
    report: &mut SnapshotReport,
) -> Result<()> {
    let stat = stat_for_path(
        walk.client,
        walk.cache,
        segments,
        walk.timeout,
        walk.cache_reads_enabled,
        &mut report.cache,
    )?;
    let is_directory = is_dir(&stat);
    let entry = SnapshotEntry::from_stat(format_path(segments), &stat);
    let include_entry = walk.options.include.includes(entry.kind());
    if include_entry && !report.push_entry(entry, walk.options) {
        return Ok(());
    }

    if is_directory && depth > 0 {
        if report.entries_full(walk.options) {
            report
                .degraded
                .push(DegradedBranch::budget_truncated(format_path(segments)));
            return Ok(());
        }
        let children = directory_entries_for_path(
            walk.client,
            walk.cache,
            segments,
            walk.timeout,
            walk.cache_reads_enabled,
            &mut report.cache,
        )?;
        for child in children {
            if child.name == b"." || child.name == b".." {
                continue;
            }
            let mut child_path = segments.to_vec();
            child_path.push(child.name);
            if report.entries_full(walk.options) {
                report
                    .degraded
                    .push(DegradedBranch::budget_truncated(format_path(&child_path)));
                continue;
            }
            if let Err(error) = collect_snapshot(walk, &child_path, depth - 1, report) {
                report
                    .degraded
                    .push(DegradedBranch::from_error(format_path(&child_path), &error));
            }
        }
    }
    Ok(())
}

fn stat_for_path(
    client: &Client,
    cache: &NamespaceCache,
    segments: &[Vec<u8>],
    timeout: Duration,
    cache_reads_enabled: bool,
    report: &mut SnapshotCacheReport,
) -> Result<Stat> {
    if cache_reads_enabled {
        if let Some(stat) = cache.stat_if_fresh(segments) {
            report.stat_hits = report.stat_hits.saturating_add(1);
            return Ok(stat);
        }
    }
    report.stat_misses = report.stat_misses.saturating_add(1);
    with_owned_fid(client, segments, timeout, |fid| {
        let stat = client.stat_timeout(fid, timeout)?;
        cache.update_stat(segments, stat.clone());
        Ok(stat)
    })
}

fn directory_entries_for_path(
    client: &Client,
    cache: &NamespaceCache,
    segments: &[Vec<u8>],
    timeout: Duration,
    cache_reads_enabled: bool,
    report: &mut SnapshotCacheReport,
) -> Result<Vec<DirEntry>> {
    if cache_reads_enabled {
        if let Some(entries) = cache.directory_if_fresh(segments) {
            report.dir_hits = report.dir_hits.saturating_add(1);
            return Ok(entries);
        }
    }
    report.dir_misses = report.dir_misses.saturating_add(1);
    with_owned_fid(client, segments, timeout, |fid| {
        client.open_timeout(fid, OREAD, timeout)?;
        let entries = read_open_directory_entries(client, fid, timeout)?;
        cache.update_directory(segments, entries.clone());
        Ok(entries)
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

pub(super) fn read_all(client: &Client, fid: r9p::fid::Fid, timeout: Duration) -> Result<Vec<u8>> {
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
