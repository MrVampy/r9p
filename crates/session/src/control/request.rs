#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlRequest {
    Status,
    Snapshot { path: String, depth: usize },
    Stat { path: String },
    List { path: String },
    Read { path: String },
}

pub fn parse_request(line: &str) -> std::result::Result<ControlRequest, String> {
    let fields = line
        .trim_end_matches(['\r', '\n'])
        .split('\t')
        .collect::<Vec<_>>();
    match fields.as_slice() {
        ["status"] => Ok(ControlRequest::Status),
        ["snapshot", path, depth] => {
            let depth = depth
                .parse::<usize>()
                .map_err(|_| format!("invalid snapshot depth {depth}"))?;
            Ok(ControlRequest::Snapshot {
                path: (*path).to_string(),
                depth,
            })
        }
        ["snapshot", path] => Ok(ControlRequest::Snapshot {
            path: (*path).to_string(),
            depth: 1,
        }),
        ["stat", path] => Ok(ControlRequest::Stat {
            path: (*path).to_string(),
        }),
        ["list", path] => Ok(ControlRequest::List {
            path: (*path).to_string(),
        }),
        ["read", path] => Ok(ControlRequest::Read {
            path: (*path).to_string(),
        }),
        [command, ..] => Err(format!("unknown session control request {command}")),
        [] => Err("empty session control request".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_request, ControlRequest};

    #[test]
    fn parses_status_request() {
        assert_eq!(parse_request("status\n"), Ok(ControlRequest::Status));
    }

    #[test]
    fn parses_snapshot_request() {
        assert_eq!(
            parse_request("snapshot\t/srv\t2\n"),
            Ok(ControlRequest::Snapshot {
                path: "/srv".to_string(),
                depth: 2
            })
        );
    }

    #[test]
    fn parses_path_requests() {
        assert_eq!(
            parse_request("stat\t/data\n"),
            Ok(ControlRequest::Stat {
                path: "/data".to_string()
            })
        );
        assert_eq!(
            parse_request("list\t/\n"),
            Ok(ControlRequest::List {
                path: "/".to_string()
            })
        );
        assert_eq!(
            parse_request("read\t/data\n"),
            Ok(ControlRequest::Read {
                path: "/data".to_string()
            })
        );
    }
}
