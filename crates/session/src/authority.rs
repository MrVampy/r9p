pub(crate) fn contained_authority(authority_boundary: &str) -> bool {
    matches!(authority_boundary, "loopback" | "unix_socket")
        || authority_boundary.starts_with("network_class:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contained_boundaries_need_no_session_credential() {
        for boundary in ["loopback", "unix_socket", "network_class:tailnet"] {
            assert!(contained_authority(boundary), "{boundary}");
        }
    }

    #[test]
    fn p9any_boundaries_use_the_session_credential() {
        for boundary in [
            "p9any:noise-xx@agents",
            "p9any:noise-xx@another-service",
            "p9any:noise-xx@agents",
        ] {
            assert!(!contained_authority(boundary), "{boundary}");
        }
    }
}
