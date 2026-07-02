use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceChange {
    pub scope: String,
    pub path: String,
    pub change_kind: String,
    pub generation: u64,
    pub event_id: String,
    pub old_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedFeedRecords {
    pub records: Vec<NamespaceChange>,
    pub cursor_advanced_to: Option<String>,
    pub cursor_missed: bool,
}

pub fn parse_namespace_change_record(line: &str) -> Option<NamespaceChange> {
    let fields = line.split('\t').collect::<Vec<_>>();
    parse_key_value_record(&fields).or_else(|| parse_positional_record(&fields))
}

pub fn parse_namespace_path(path: &str) -> Result<Vec<Vec<u8>>> {
    if !path.starts_with('/') {
        return Err(Error::new(
            libc::EINVAL,
            format!("namespace change path must be absolute: {path}"),
        ));
    }
    Ok(path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.as_bytes().to_vec())
        .collect())
}

pub fn scope_matches(configured_scope: Option<&str>, event_scope: &str) -> bool {
    event_scope == "shared"
        || configured_scope
            .map(|scope| scope == event_scope)
            .unwrap_or(true)
}

pub fn feed_poll_path(
    base_path: &str,
    since_event_id: Option<&str>,
    cursor_template: Option<&str>,
) -> String {
    match (since_event_id, cursor_template) {
        (Some(event_id), Some(template)) => template.replace("{event_id}", event_id),
        _ => base_path.to_string(),
    }
}

pub fn select_feed_records(
    records: Vec<NamespaceChange>,
    since_event_id: Option<&str>,
    cursor_template_configured: bool,
) -> SelectedFeedRecords {
    if cursor_template_configured {
        return SelectedFeedRecords {
            records,
            cursor_advanced_to: None,
            cursor_missed: false,
        };
    }
    let Some(cursor) = since_event_id else {
        return SelectedFeedRecords {
            records,
            cursor_advanced_to: None,
            cursor_missed: false,
        };
    };
    let Some(index) = records.iter().position(|record| record.event_id == cursor) else {
        let cursor_advanced_to = records.last().map(|record| record.event_id.clone());
        return SelectedFeedRecords {
            cursor_missed: !records.is_empty(),
            records: Vec::new(),
            cursor_advanced_to,
        };
    };
    SelectedFeedRecords {
        records: records.into_iter().skip(index + 1).collect(),
        cursor_advanced_to: None,
        cursor_missed: false,
    }
}

fn parse_key_value_record(fields: &[&str]) -> Option<NamespaceChange> {
    let mut scope = None;
    let mut path = None;
    let mut change_kind = None;
    let mut generation = None;
    let mut event_id = None;
    let mut old_path = None;
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "scope" => scope = Some(value.to_string()),
            "path" => path = Some(value.to_string()),
            "change_kind" | "kind" => change_kind = Some(value.to_string()),
            "generation" => generation = value.parse::<u64>().ok(),
            "event_id" => event_id = Some(value.to_string()),
            "old_path" | "from" => old_path = Some(value.to_string()),
            _ => {}
        }
    }
    Some(NamespaceChange {
        scope: scope?,
        path: path?,
        change_kind: change_kind?,
        generation: generation?,
        event_id: event_id?,
        old_path,
    })
}

fn parse_positional_record(fields: &[&str]) -> Option<NamespaceChange> {
    match fields {
        ["namespace_change", event_id, generation, scope, change_kind, path] => {
            Some(NamespaceChange {
                scope: (*scope).to_string(),
                path: (*path).to_string(),
                change_kind: (*change_kind).to_string(),
                generation: generation.parse().ok()?,
                event_id: (*event_id).to_string(),
                old_path: None,
            })
        }
        ["namespace_change", event_id, generation, scope, "renamed", old_path, path] => {
            Some(NamespaceChange {
                scope: (*scope).to_string(),
                path: (*path).to_string(),
                change_kind: "renamed".to_string(),
                generation: generation.parse().ok()?,
                event_id: (*event_id).to_string(),
                old_path: Some((*old_path).to_string()),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        feed_poll_path, parse_namespace_change_record, parse_namespace_path, scope_matches,
        select_feed_records, NamespaceChange,
    };

    #[test]
    fn parses_key_value_namespace_change_record() {
        let record = parse_namespace_change_record(
            "namespace_change\tevent_id=e1\tgeneration=42\tscope=shared\tchange_kind=created\tpath=/tree/x",
        )
        .expect("record should parse");

        assert_eq!(record.event_id, "e1");
        assert_eq!(record.generation, 42);
        assert_eq!(record.scope, "shared");
        assert_eq!(record.change_kind, "created");
        assert_eq!(record.path, "/tree/x");
    }

    #[test]
    fn parses_positional_rename_record() {
        let record = parse_namespace_change_record(
            "namespace_change\te2\t43\tsession:abc\trenamed\t/tree/old\t/tree/new",
        )
        .expect("record should parse");

        assert_eq!(record.old_path.as_deref(), Some("/tree/old"));
        assert_eq!(record.path, "/tree/new");
    }

    #[test]
    fn namespace_paths_are_absolute() {
        assert_eq!(
            parse_namespace_path("/tree/status").expect("path should parse"),
            vec![b"tree".to_vec(), b"status".to_vec()]
        );
        assert!(parse_namespace_path("tree/status").is_err());
    }

    #[test]
    fn change_feed_scope_matches_shared_or_configured_scope() {
        assert!(scope_matches(Some("session:a"), "shared"));
        assert!(scope_matches(Some("session:a"), "session:a"));
        assert!(!scope_matches(Some("session:a"), "session:b"));
        assert!(scope_matches(None, "session:b"));
    }

    #[test]
    fn feed_poll_path_advances_with_since_cursor() {
        assert_eq!(
            feed_poll_path(
                "/feeds/namespace",
                Some("event-7"),
                Some("/feeds/namespace-after/{event_id}"),
            ),
            "/feeds/namespace-after/event-7"
        );
        assert_eq!(
            feed_poll_path("/feeds/namespace", Some("event-7"), None),
            "/feeds/namespace"
        );
        assert_eq!(
            feed_poll_path("/feeds/namespace", None, None),
            "/feeds/namespace"
        );
    }

    #[test]
    fn select_feed_records_skips_seen_cursor() {
        let records = vec![record("a"), record("b"), record("c")];
        let selected = select_feed_records(records, Some("b"), false);

        assert_eq!(selected.records, vec![record("c")]);
        assert_eq!(selected.cursor_advanced_to, None);
        assert!(!selected.cursor_missed);
    }

    #[test]
    fn select_feed_records_reports_missed_recent_cursor() {
        let records = vec![record("b"), record("c")];
        let selected = select_feed_records(records, Some("a"), false);

        assert!(selected.records.is_empty());
        assert_eq!(selected.cursor_advanced_to.as_deref(), Some("c"));
        assert!(selected.cursor_missed);
    }

    fn record(event_id: &str) -> NamespaceChange {
        NamespaceChange {
            scope: "shared".to_string(),
            path: "/tree/x".to_string(),
            change_kind: "modified".to_string(),
            generation: 1,
            event_id: event_id.to_string(),
            old_path: None,
        }
    }
}
