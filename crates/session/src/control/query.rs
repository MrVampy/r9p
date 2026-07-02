use super::{freshness::ResponseFreshness, json, options, snapshot, status_json, ControlConfig};
use crate::{feed::FeedState, Client, NamespaceCache, Result};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum QueryRequest {
    Status,
    Snapshot {
        path: String,
        depth: usize,
        options: options::SnapshotOptions,
    },
    Stat {
        path: String,
    },
    List {
        path: String,
    },
    Read {
        path: String,
    },
}

pub(super) fn parse_json(data: &[u8]) -> std::result::Result<QueryRequest, String> {
    let value = serde_json::from_slice::<Value>(data)
        .map_err(|error| format!("invalid query JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "query must be a JSON object".to_string())?;
    let op = string_field(object, "op")?;
    match op {
        "status" => {
            reject_unknown_fields(object, &["op"])?;
            Ok(QueryRequest::Status)
        }
        "snapshot" => {
            reject_unknown_fields(
                object,
                &["op", "path", "depth", "include", "fields", "budget"],
            )?;
            Ok(QueryRequest::Snapshot {
                path: optional_string_field(object, "path")?.unwrap_or_else(|| "/".to_string()),
                depth: optional_usize_field(object, "depth")?.unwrap_or(1),
                options: snapshot_options(object)?,
            })
        }
        "stat" => {
            reject_unknown_fields(object, &["op", "path"])?;
            Ok(QueryRequest::Stat {
                path: optional_string_field(object, "path")?.unwrap_or_else(|| "/".to_string()),
            })
        }
        "list" => {
            reject_unknown_fields(object, &["op", "path"])?;
            Ok(QueryRequest::List {
                path: optional_string_field(object, "path")?.unwrap_or_else(|| "/".to_string()),
            })
        }
        "read" => {
            reject_unknown_fields(object, &["op", "path"])?;
            Ok(QueryRequest::Read {
                path: string_field(object, "path")?.to_string(),
            })
        }
        _ => Err(format!("unknown query op {op}")),
    }
}

pub(super) fn response_json(
    client: &Client,
    config: &ControlConfig,
    feed_state: &FeedState,
    cache: &NamespaceCache,
    session_epoch: &str,
    request: QueryRequest,
) -> String {
    match response_json_result(client, config, feed_state, cache, session_epoch, request) {
        Ok(response) => response,
        Err(error) => json::error_response("session_error", error.message()),
    }
}

fn response_json_result(
    client: &Client,
    config: &ControlConfig,
    feed_state: &FeedState,
    cache: &NamespaceCache,
    session_epoch: &str,
    request: QueryRequest,
) -> Result<String> {
    let freshness = ResponseFreshness::from_feed(session_epoch, feed_state);
    let cache_reads_enabled = feed_state.snapshot().state == "connected";
    match request {
        QueryRequest::Status => status_json(client, config, feed_state, cache, session_epoch),
        QueryRequest::Snapshot {
            path,
            depth,
            options,
        } => snapshot::snapshot_json_with_options(
            client,
            cache,
            &path,
            depth,
            config.request_timeout,
            &options,
            cache_reads_enabled,
            &freshness,
        ),
        QueryRequest::Stat { path } => snapshot::stat_json(
            client,
            cache,
            &path,
            config.request_timeout,
            cache_reads_enabled,
            &freshness,
        ),
        QueryRequest::List { path } => snapshot::list_json(
            client,
            cache,
            &path,
            config.request_timeout,
            cache_reads_enabled,
            &freshness,
        ),
        QueryRequest::Read { path } => {
            snapshot::read_json(client, &path, config.request_timeout, &freshness)
        }
    }
}

fn snapshot_options(
    object: &Map<String, Value>,
) -> std::result::Result<options::SnapshotOptions, String> {
    Ok(options::SnapshotOptions {
        include: optional_include_field(object, "include")?.unwrap_or(options::IncludeKind::Both),
        fields: optional_fields_field(object, "fields")?.unwrap_or_else(options::EntryFields::all),
        budget: optional_usize_field(object, "budget")?,
    })
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

fn optional_include_field(
    object: &Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<options::IncludeKind>, String> {
    let Some(value) = optional_string_field(object, field)? else {
        return Ok(None);
    };
    options::IncludeKind::from_str(&value)
        .map(Some)
        .ok_or_else(|| format!("query field {field} must be one of both, dirs, files"))
}

fn optional_fields_field(
    object: &Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<options::EntryFields>, String> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("query field {field} must be an array"))?;
    let mut names = Vec::with_capacity(array.len());
    for item in array {
        let Some(name) = item.as_str() else {
            return Err(format!("query field {field} must contain strings"));
        };
        names.push(name.to_string());
    }
    options::EntryFields::from_names(&names).map(Some)
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
    use crate::control::{options::IncludeKind, query::QueryRequest};

    #[test]
    fn parses_snapshot_query() {
        let request =
            parse_json(br#"{"op":"snapshot","path":"/srv","depth":2}"#).expect("snapshot query");
        match request {
            QueryRequest::Snapshot {
                path,
                depth,
                options,
            } => {
                assert_eq!(path, "/srv");
                assert_eq!(depth, 2);
                assert_eq!(options.include, IncludeKind::Both);
                assert_eq!(options.budget, None);
            }
            other => panic!("unexpected request {other:?}"),
        }
    }

    #[test]
    fn parses_snapshot_query_options() {
        let request = parse_json(
            br#"{"op":"snapshot","path":"/srv","depth":2,"include":"files","fields":["path","kind","length"],"budget":4}"#,
        )
        .expect("snapshot query");
        match request {
            QueryRequest::Snapshot {
                path,
                depth,
                options,
            } => {
                assert_eq!(path, "/srv");
                assert_eq!(depth, 2);
                assert_eq!(options.include, IncludeKind::Files);
                assert_eq!(options.budget, Some(4));
                assert!(options.fields.path());
                assert!(!options.fields.qid());
            }
            other => panic!("unexpected request {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = parse_json(br#"{"op":"stat","path":"/","legacy":true}"#)
            .expect_err("unknown fields should fail");
        assert!(error.contains("unknown query field legacy"));
    }
}
