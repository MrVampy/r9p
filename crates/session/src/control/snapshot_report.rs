use super::{json, options::SnapshotOptions, snapshot_path::kind};
use r9p::stat::Stat;

#[derive(Default)]
pub(super) struct SnapshotReport {
    pub(super) entries: Vec<SnapshotEntry>,
    pub(super) degraded: Vec<DegradedBranch>,
    pub(super) cache: SnapshotCacheReport,
}

#[derive(Default)]
pub(super) struct SnapshotCacheReport {
    pub(super) stat_hits: usize,
    pub(super) stat_misses: usize,
    pub(super) dir_hits: usize,
    pub(super) dir_misses: usize,
}

pub(super) struct SnapshotEntry {
    path: String,
    name: String,
    kind: &'static str,
    qid_path: u64,
    qid_version: u32,
    qid_type: u8,
    mode: u32,
    length: u64,
    mtime: u32,
}

pub(super) struct DegradedBranch {
    path: String,
    reason: &'static str,
    message: String,
}

impl SnapshotReport {
    pub(super) fn push_entry(&mut self, entry: SnapshotEntry, options: &SnapshotOptions) -> bool {
        if self.entries_full(options) {
            self.degraded
                .push(DegradedBranch::budget_truncated(entry.path));
            return false;
        }
        self.entries.push(entry);
        true
    }

    pub(super) fn entries_full(&self, options: &SnapshotOptions) -> bool {
        options
            .budget
            .is_some_and(|budget| self.entries.len() >= budget)
    }
}

impl SnapshotEntry {
    pub(super) fn from_stat(path: String, stat: &Stat) -> Self {
        Self {
            path,
            name: json::bytes_lossy(&stat.name),
            kind: kind(stat),
            qid_path: stat.qid.path,
            qid_version: stat.qid.version,
            qid_type: stat.qid.qtype,
            mode: stat.mode,
            length: stat.length,
            mtime: stat.mtime,
        }
    }

    pub(super) fn kind(&self) -> &'static str {
        self.kind
    }
}

impl DegradedBranch {
    pub(super) fn from_error(path: String, error: &crate::Error) -> Self {
        Self {
            path,
            reason: degraded_reason(error.errno),
            message: error.message().to_string(),
        }
    }

    pub(super) fn budget_truncated(path: String) -> Self {
        Self {
            path,
            reason: "budget_truncated",
            message: "snapshot entry budget reached".to_string(),
        }
    }
}

pub(super) fn push_snapshot_entry(
    out: &mut String,
    entry: &SnapshotEntry,
    options: &SnapshotOptions,
) {
    let mut first = true;
    out.push('{');
    push_entry_string_field(out, &mut first, "path", &entry.path, options.fields.path());
    push_entry_string_field(out, &mut first, "name", &entry.name, options.fields.name());
    push_entry_string_field(out, &mut first, "kind", entry.kind, options.fields.kind());
    if options.fields.qid() {
        push_entry_separator(out, &mut first);
        out.push_str("\"qid\":{\"path\":");
        out.push_str(&entry.qid_path.to_string());
        out.push_str(",\"version\":");
        out.push_str(&entry.qid_version.to_string());
        out.push_str(",\"type\":");
        out.push_str(&entry.qid_type.to_string());
        out.push('}');
    }
    push_entry_number_field(
        out,
        &mut first,
        "mode",
        entry.mode.into(),
        options.fields.mode(),
    );
    push_entry_number_field(
        out,
        &mut first,
        "length",
        entry.length,
        options.fields.length(),
    );
    push_entry_number_field(
        out,
        &mut first,
        "mtime",
        entry.mtime.into(),
        options.fields.mtime(),
    );
    out.push('}');
}

pub(super) fn push_degraded_branch(out: &mut String, branch: &DegradedBranch) {
    out.push_str("{\"path\":");
    json::push_string(out, &branch.path);
    out.push_str(",\"reason\":");
    json::push_string(out, branch.reason);
    out.push_str(",\"message\":");
    json::push_string(out, &branch.message);
    out.push('}');
}

pub(super) fn push_cache_report(out: &mut String, report: &SnapshotCacheReport, enabled: bool) {
    out.push_str("{\"enabled\":");
    out.push_str(if enabled { "true" } else { "false" });
    out.push_str(",\"stat_hits\":");
    out.push_str(&report.stat_hits.to_string());
    out.push_str(",\"stat_misses\":");
    out.push_str(&report.stat_misses.to_string());
    out.push_str(",\"dir_hits\":");
    out.push_str(&report.dir_hits.to_string());
    out.push_str(",\"dir_misses\":");
    out.push_str(&report.dir_misses.to_string());
    out.push('}');
}

fn push_entry_string_field(
    out: &mut String,
    first: &mut bool,
    name: &str,
    value: &str,
    include: bool,
) {
    if include {
        push_entry_separator(out, first);
        json::push_string(out, name);
        out.push(':');
        json::push_string(out, value);
    }
}

fn push_entry_number_field(
    out: &mut String,
    first: &mut bool,
    name: &str,
    value: u64,
    include: bool,
) {
    if include {
        push_entry_separator(out, first);
        json::push_string(out, name);
        out.push(':');
        out.push_str(&value.to_string());
    }
}

fn push_entry_separator(out: &mut String, first: &mut bool) {
    if *first {
        *first = false;
    } else {
        out.push(',');
    }
}

fn degraded_reason(errno: i32) -> &'static str {
    match errno {
        libc::EACCES | libc::EPERM => "denied",
        libc::ENOENT | libc::ENOTDIR => "missing",
        libc::ETIMEDOUT | libc::EAGAIN => "timed_out",
        libc::ESTALE => "stale",
        _ => "unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::degraded_reason;

    #[test]
    fn maps_errno_to_degraded_reason() {
        assert_eq!(degraded_reason(libc::EACCES), "denied");
        assert_eq!(degraded_reason(libc::ENOENT), "missing");
        assert_eq!(degraded_reason(libc::ETIMEDOUT), "timed_out");
        assert_eq!(degraded_reason(libc::ESTALE), "stale");
    }
}
