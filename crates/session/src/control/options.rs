#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SnapshotOptions {
    pub include: IncludeKind,
    pub fields: EntryFields,
    pub budget: Option<usize>,
    pub freshness: FreshnessMode,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            include: IncludeKind::Both,
            fields: EntryFields::all(),
            budget: None,
            freshness: FreshnessMode::CachedOk,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FreshnessMode {
    CachedOk,
    MustRevalidate,
}

impl FreshnessMode {
    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "cached_ok" => Some(Self::CachedOk),
            "must_revalidate" => Some(Self::MustRevalidate),
            _ => None,
        }
    }

    pub(super) fn allows_cache_reads(self) -> bool {
        matches!(self, Self::CachedOk)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IncludeKind {
    Both,
    Directories,
    Files,
}

impl IncludeKind {
    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "both" => Some(Self::Both),
            "dirs" => Some(Self::Directories),
            "files" => Some(Self::Files),
            _ => None,
        }
    }

    pub(super) fn includes(self, kind: &str) -> bool {
        match self {
            Self::Both => true,
            Self::Directories => kind == "dir",
            Self::Files => kind != "dir",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct EntryFields {
    bits: u16,
}

impl EntryFields {
    const PATH: u16 = 1 << 0;
    const NAME: u16 = 1 << 1;
    const KIND: u16 = 1 << 2;
    const QID: u16 = 1 << 3;
    const MODE: u16 = 1 << 4;
    const LENGTH: u16 = 1 << 5;
    const MTIME: u16 = 1 << 6;

    pub(super) fn all() -> Self {
        Self {
            bits: Self::PATH
                | Self::NAME
                | Self::KIND
                | Self::QID
                | Self::MODE
                | Self::LENGTH
                | Self::MTIME,
        }
    }

    pub(super) fn from_names(names: &[String]) -> Result<Self, String> {
        if names.is_empty() {
            return Err("query field fields must not be empty".to_string());
        }
        let mut bits = 0;
        for name in names {
            bits |= match name.as_str() {
                "path" => Self::PATH,
                "name" => Self::NAME,
                "kind" => Self::KIND,
                "qid" => Self::QID,
                "mode" => Self::MODE,
                "length" => Self::LENGTH,
                "mtime" => Self::MTIME,
                _ => return Err(format!("unknown snapshot entry field {name}")),
            };
        }
        Ok(Self { bits })
    }

    pub(super) fn path(self) -> bool {
        self.bits & Self::PATH != 0
    }

    pub(super) fn name(self) -> bool {
        self.bits & Self::NAME != 0
    }

    pub(super) fn kind(self) -> bool {
        self.bits & Self::KIND != 0
    }

    pub(super) fn qid(self) -> bool {
        self.bits & Self::QID != 0
    }

    pub(super) fn mode(self) -> bool {
        self.bits & Self::MODE != 0
    }

    pub(super) fn length(self) -> bool {
        self.bits & Self::LENGTH != 0
    }

    pub(super) fn mtime(self) -> bool {
        self.bits & Self::MTIME != 0
    }
}

#[cfg(test)]
mod tests {
    use super::{EntryFields, FreshnessMode, IncludeKind};

    #[test]
    fn include_kind_parses_contract_names() {
        assert_eq!(IncludeKind::from_str("both"), Some(IncludeKind::Both));
        assert_eq!(
            IncludeKind::from_str("dirs"),
            Some(IncludeKind::Directories)
        );
        assert_eq!(IncludeKind::from_str("files"), Some(IncludeKind::Files));
        assert_eq!(IncludeKind::from_str("all"), None);
    }

    #[test]
    fn freshness_mode_parses_contract_names() {
        assert_eq!(
            FreshnessMode::from_str("cached_ok"),
            Some(FreshnessMode::CachedOk)
        );
        assert_eq!(
            FreshnessMode::from_str("must_revalidate"),
            Some(FreshnessMode::MustRevalidate)
        );
        assert_eq!(FreshnessMode::from_str("sync"), None);
    }

    #[test]
    fn entry_fields_reject_unknown_fields() {
        let error = EntryFields::from_names(&["path".to_string(), "size".to_string()])
            .expect_err("unknown fields should fail");
        assert!(error.contains("unknown snapshot entry field size"));
    }
}
