use std::time::{SystemTime, UNIX_EPOCH};

use r9p::{
    qid::{Qid, DMAPPEND, DMDIR},
    stat::Stat,
};

use crate::errors::{cli_error, CliResult};
use crate::{DMAUTH, DMDEVICE, DMEXCL, DMNAMEDPIPE, DMSOCKET, DMSYMLINK};

pub(crate) fn format_stat(stat: &Stat) -> String {
    format!(
        "'{}' '{}' '{}' '{}' q ({:016x} {} {}) m 0{:o} at {} mt {} l {} t {} d {}",
        text(&stat.name),
        text(&stat.uid),
        text(&stat.gid),
        text(&stat.muid),
        stat.qid.path,
        stat.qid.version,
        qid_type_string(stat.qid.qtype),
        stat.mode,
        stat.atime,
        stat.mtime,
        stat.length,
        stat.type_,
        stat.dev
    )
}

pub(crate) fn format_version(msize: u32, version: &[u8]) -> String {
    format!("version={} msize={}", text(version), msize)
}

pub(crate) fn format_attach(qid: Qid) -> String {
    format!("attached qid={}", format_qid(qid))
}

pub(crate) fn format_qid(qid: Qid) -> String {
    let kind = if qid.is_dir() { "dir" } else { "file" };
    format!("{kind}/{}/{}", qid.version, qid.path)
}

pub(crate) fn qid_type_string(qtype: u8) -> String {
    let mut out = String::new();
    if qtype & 0x80 != 0 {
        out.push('d');
    }
    if qtype & 0x40 != 0 {
        out.push('a');
    }
    if qtype & 0x20 != 0 {
        out.push('l');
    }
    if qtype & 0x08 != 0 {
        out.push('A');
    }
    out
}

pub(crate) fn mode_string(mode: u32) -> String {
    let mut out = String::with_capacity(11);
    out.push(if mode & DMDIR != 0 {
        'd'
    } else if mode & DMAPPEND != 0 {
        'a'
    } else if mode & DMAUTH != 0 {
        'A'
    } else if mode & DMDEVICE != 0 {
        'D'
    } else if mode & DMSOCKET != 0 {
        'S'
    } else if mode & DMNAMEDPIPE != 0 {
        'P'
    } else {
        '-'
    });
    out.push(if mode & DMEXCL != 0 {
        'l'
    } else if mode & DMSYMLINK != 0 {
        'L'
    } else {
        '-'
    });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        out.push(if bits & 4 != 0 { 'r' } else { '-' });
        out.push(if bits & 2 != 0 { 'w' } else { '-' });
        out.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    out
}

pub(crate) fn time_string(mtime: u32) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let mtime_u64 = u64::from(mtime);
    let components = utc_components(mtime_u64);
    if now.saturating_sub(mtime_u64) < 6 * 30 * 24 * 60 * 60 {
        format!(
            "{} {:>2} {:02}:{:02}",
            month_name(components.month),
            components.day,
            components.hour,
            components.minute
        )
    } else {
        format!(
            "{} {:>2} {:>5}",
            month_name(components.month),
            components.day,
            components.year
        )
    }
}

#[derive(Debug)]
pub(crate) struct DateTime {
    pub(crate) year: i32,
    pub(crate) month: u32,
    pub(crate) day: u32,
    pub(crate) hour: u32,
    pub(crate) minute: u32,
}

pub(crate) fn utc_components(seconds: u64) -> DateTime {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let secs_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    DateTime {
        year,
        month,
        day,
        hour: u32::try_from(secs_of_day / 3600).unwrap_or(0),
        minute: u32::try_from((secs_of_day % 3600) / 60).unwrap_or(0),
    }
}

pub(crate) fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (
        i32::try_from(year).unwrap_or(i32::MAX),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

pub(crate) fn month_name(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "???",
    }
}

pub(crate) fn quote_name(bytes: &[u8]) -> String {
    let value = text(bytes);
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '+'))
    {
        return value;
    }
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

pub(crate) fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn hex_decode(raw: &str) -> CliResult<Vec<u8>> {
    let bytes = raw.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(cli_error("odd hex length"));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

pub(crate) fn hex_value(byte: u8) -> CliResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(cli_error(format!("invalid hex byte {}", byte as char))),
    }
}
