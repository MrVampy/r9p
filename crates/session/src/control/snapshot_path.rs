use super::json;
use crate::{is_dir, is_symlink};
use r9p::stat::Stat;

pub(super) fn parse_namespace_path(path: &str) -> Vec<Vec<u8>> {
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.as_bytes().to_vec())
        .collect()
}

pub(super) fn format_path(segments: &[Vec<u8>]) -> String {
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

pub(super) fn kind(stat: &Stat) -> &'static str {
    if is_dir(stat) {
        "dir"
    } else if is_symlink(stat) {
        "symlink"
    } else {
        "file"
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
