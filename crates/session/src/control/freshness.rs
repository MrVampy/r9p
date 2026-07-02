use super::json;
use crate::feed::FeedState;

#[derive(Clone, Debug)]
pub(super) struct ResponseFreshness {
    pub session_epoch: String,
    pub feed_generation: Option<u64>,
    pub fresh_instance: bool,
}

impl ResponseFreshness {
    pub(super) fn from_feed(session_epoch: &str, feed_state: &FeedState) -> Self {
        let feed = feed_state.snapshot();
        Self {
            session_epoch: session_epoch.to_string(),
            feed_generation: feed.last_generation,
            fresh_instance: feed.fresh_instance,
        }
    }
}

pub(super) fn push_json(out: &mut String, freshness: &ResponseFreshness) {
    out.push_str("{\"state\":\"fresh\",\"session_epoch\":");
    json::push_string(out, &freshness.session_epoch);
    out.push_str(",\"feed_generation\":");
    match freshness.feed_generation {
        Some(generation) => out.push_str(&generation.to_string()),
        None => out.push_str("null"),
    }
    out.push_str(",\"fresh_instance\":");
    out.push_str(if freshness.fresh_instance {
        "true"
    } else {
        "false"
    });
    out.push('}');
}
