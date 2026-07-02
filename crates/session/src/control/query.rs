use super::{json, snapshot, status_json, ControlConfig, ControlRequest};
use crate::{Client, Result};
use serde_json::{Map, Value};

pub(super) fn parse_json(data: &[u8]) -> std::result::Result<ControlRequest, String> {
    let value = serde_json::from_slice::<Value>(data)
        .map_err(|error| format!("invalid query JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "query must be a JSON object".to_string())?;
    let op = string_field(object, "op")?;
    match op {
        "status" => {
            reject_unknown_fields(object, &["op"])?;
            Ok(ControlRequest::Status)
        }
        "snapshot" => {
            reject_unknown_fields(object, &["op", "path", "depth"])?;
            Ok(ControlRequest::Snapshot {
                path: optional_string_field(object, "path")?.unwrap_or_else(|| "/".to_string()),
                depth: optional_usize_field(object, "depth")?.unwrap_or(1),
            })
        }
        "stat" => {
            reject_unknown_fields(object, &["op", "path"])?;
            Ok(ControlRequest::Stat {
                path: optional_string_field(object, "path")?.unwrap_or_else(|| "/".to_string()),
            })
        }
        "list" => {
            reject_unknown_fields(object, &["op", "path"])?;
            Ok(ControlRequest::List {
                path: optional_string_field(object, "path")?.unwrap_or_else(|| "/".to_string()),
            })
        }
        "read" => {
            reject_unknown_fields(object, &["op", "path"])?;
            Ok(ControlRequest::Read {
                path: string_field(object, "path")?.to_string(),
            })
        }
        _ => Err(format!("unknown query op {op}")),
    }
}

pub(super) fn response_json(
    client: &Client,
    config: &ControlConfig,
    request: ControlRequest,
) -> String {
    match response_json_result(client, config, request) {
        Ok(response) => response,
        Err(error) => json::error_response("session_error", error.message()),
    }
}

fn response_json_result(
    client: &Client,
    config: &ControlConfig,
    request: ControlRequest,
) -> Result<String> {
    match request {
        ControlRequest::Status => status_json(client, config),
        ControlRequest::Snapshot { path, depth } => {
            snapshot::snapshot_json(client, &path, depth, config.request_timeout)
        }
        ControlRequest::Stat { path } => snapshot::stat_json(client, &path, config.request_timeout),
        ControlRequest::List { path } => snapshot::list_json(client, &path, config.request_timeout),
        ControlRequest::Read { path } => snapshot::read_json(client, &path, config.request_timeout),
    }
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> std::result::Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("query field {field} must be a string"))
}

fn optional_string_field(
    object: &Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<String>, String> {
    match object.get(field) {
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| format!("query field {field} must be a string")),
        None => Ok(None),
    }
}

fn optional_usize_field(
    object: &Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<usize>, String> {
    match object.get(field) {
        Some(value) => {
            let value = value
                .as_u64()
                .ok_or_else(|| format!("query field {field} must be an unsigned integer"))?;
            usize::try_from(value)
                .map(Some)
                .map_err(|_| format!("query field {field} is too large"))
        }
        None => Ok(None),
    }
}

fn reject_unknown_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
) -> std::result::Result<(), String> {
    for field in object.keys() {
        if !allowed.iter().any(|allowed| allowed == field) {
            return Err(format!("unknown query field {field}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_json;
    use crate::control::ControlRequest;

    #[test]
    fn parses_snapshot_query() {
        assert_eq!(
            parse_json(br#"{"op":"snapshot","path":"/srv","depth":2}"#),
            Ok(ControlRequest::Snapshot {
                path: "/srv".to_string(),
                depth: 2
            })
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = parse_json(br#"{"op":"stat","path":"/","legacy":true}"#)
            .expect_err("unknown fields should fail");
        assert!(error.contains("unknown query field legacy"));
    }
}
