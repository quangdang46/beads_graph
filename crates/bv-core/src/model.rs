//! Frozen data model for beads issues (api-freeze-v1).
//!
//! Ported 1:1 from upstream `pkg/model/types.go` @ 9ace029.
//! Field names, JSON tags, and semantics are part of the compatibility
//! contract — changes here require a plan edit + CHANGELOG "API change"
//! entry (additive-only during parity).

use serde::{Deserialize, Serialize};

// /// Serialize a timestamp the way Go marshals a `time.Time`: UTC, RFC3339 with
/// trailing zeros trimmed from the fractional part. The loader stores the raw
/// source text, so values like `...206355500Z` would otherwise pass through
/// where Go emits `...2063555Z`.
pub fn serialize_go_time<S: serde::Serializer>(
    v: &Option<String>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match v {
        None => s.serialize_none(),
        Some(raw) => match raw.parse::<jiff::Timestamp>() {
            Ok(ts) => {
                let text = ts.to_string();
                // jiff already appends the "Z"; only the fractional part
                // needs Go's trailing-zero trim.
                let out = match text.find('.') {
                    Some(dot) => {
                        let frac = text[dot + 1..].trim_end_matches('0');
                        if frac.is_empty() {
                            text[..dot].to_string()
                        } else {
                            format!("{}.{}", &text[..dot], frac)
                        }
                    }
                    None => text,
                };
                s.serialize_str(&out)
            }
            // Unparseable input is passed through rather than dropped, so a
            // malformed record stays visible instead of vanishing.
            Err(_) => s.serialize_str(raw),
        },
    }
}
/// Issue status. Exactly the 10 values Go recognizes (`model.Status.IsValid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Status {
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "deferred")]
    Deferred,
    #[serde(rename = "draft")]
    Draft,
    #[serde(rename = "pinned")]
    Pinned,
    #[serde(rename = "hooked")]
    Hooked,
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "closed")]
    Closed,
    #[serde(rename = "tombstone")]
    Tombstone,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::InProgress => "in_progress",
            Status::Blocked => "blocked",
            Status::Deferred => "deferred",
            Status::Draft => "draft",
            Status::Pinned => "pinned",
            Status::Hooked => "hooked",
            Status::Review => "review",
            Status::Closed => "closed",
            Status::Tombstone => "tombstone",
        }
    }

    /// Go: `IsClosed()` — only exactly `closed`.
    pub fn is_closed(self) -> bool {
        self == Status::Closed
    }

    /// Go: `IsOpen()` — open OR in_progress.
    pub fn is_open(self) -> bool {
        matches!(self, Status::Open | Status::InProgress)
    }

    /// Go: `IsTombstone()`.
    pub fn is_tombstone(self) -> bool {
        self == Status::Tombstone
    }

    /// Parse from raw string (trim+lowercase like Go normalizeIssueStatus).
    pub fn parse(raw: &str) -> Option<Status> {
        match raw.trim().to_lowercase().as_str() {
            "open" => Some(Status::Open),
            "in_progress" => Some(Status::InProgress),
            "blocked" => Some(Status::Blocked),
            "deferred" => Some(Status::Deferred),
            "draft" => Some(Status::Draft),
            "pinned" => Some(Status::Pinned),
            "hooked" => Some(Status::Hooked),
            "review" => Some(Status::Review),
            "closed" => Some(Status::Closed),
            "tombstone" => Some(Status::Tombstone),
            _ => None,
        }
    }
}

/// Dependency type. Legacy compat: an EMPTY type string is blocking,
/// same as `blocks` (Go: `DependencyType.IsBlocking`).
/// `Other` carries a custom relationship verbatim. Go models the type as a
/// plain `string` whose `IsValid` accepts anything non-blank (types.go:431), so
/// a br workspace's own relationship type survives loading and is hashed as
/// itself. Without this variant an unrecognized type was mapped onto
/// `Related`, so `data_hash` hashed `"related"` where the oracle hashes the
/// real string and the two diverged.
///
/// The variant costs `Copy`, which is why the issue recorded the change as
/// touching ~20 call sites. The methods below take `self` by reference and
/// return borrowed strings, so the common path is unaffected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum DependencyType {
    #[default]
    Blocks,
    ConditionalBlocks,
    WaitsFor,
    Related,
    ParentChild,
    DiscoveredFrom,
    /// A relationship type `br` defines and this build does not know.
    Other(String),
}

impl DependencyType {
    /// The wire form, which for `Other` is the type `br` wrote.
    pub fn as_str(&self) -> &str {
        match self {
            DependencyType::Blocks => "blocks",
            DependencyType::ConditionalBlocks => "conditional-blocks",
            DependencyType::WaitsFor => "waits-for",
            DependencyType::Related => "related",
            DependencyType::ParentChild => "parent-child",
            DependencyType::DiscoveredFrom => "discovered-from",
            DependencyType::Other(raw) => raw,
        }
    }

    /// Go `DependencyType.IsBlocking` (pkg/model/types.go:439): `""`, `blocks`,
    /// `conditional-blocks` and `waits-for` all block. `""` reaches here as
    /// [`DependencyType::Blocks`] because [`DependencyType::parse`] maps it.
    ///
    /// The two typed conditional relationships are blocking even though their
    /// names do not say so — they gate readiness exactly like `blocks` does.
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            DependencyType::Blocks | DependencyType::ConditionalBlocks | DependencyType::WaitsFor
        )
    }

    /// Parse from raw JSONL string; "" maps to Blocks (legacy default).
    pub fn parse(raw: &str) -> Self {
        match raw {
            "" | "blocks" => DependencyType::Blocks,
            "conditional-blocks" => DependencyType::ConditionalBlocks,
            "waits-for" => DependencyType::WaitsFor,
            "related" => DependencyType::Related,
            "parent-child" => DependencyType::ParentChild,
            "discovered-from" => DependencyType::DiscoveredFrom,
            // Go's `IsValid` accepts any non-blank type so br's custom
            // relationships survive loading, and `IsBlocking` is false for
            // them. A non-blocking variant is therefore the faithful mapping
            // for anything unrecognized; `raw_is_valid` records the difference
            // so the loader can still tell a known type from a custom one.
            // Go's `IsValid` accepts any non-blank type so br's custom
            // relationships survive loading, and `IsBlocking` is false for
            // them. `Other` is the faithful mapping: non-blocking, and it
            // round-trips the original string instead of rewriting it.
            other => DependencyType::Other(other.to_string()),
        }
    }

    /// Go `DependencyType.IsValid` (pkg/model/types.go:431) is a non-blank
    /// test, which is what keeps br's custom relationships loadable.
    ///
    /// The loader additionally accepts the **legacy empty** type. Go rejects
    /// empty there, but Go's `IsBlocking` treats `""` as blocking, and
    /// pre-typed beads data omits `type` entirely — so rejecting it here
    /// would drop real issues rather than model them. Whitespace-only is
    /// still invalid.
    pub fn raw_is_valid(raw: &str) -> bool {
        raw.is_empty() || !raw.trim().is_empty()
    }
}

impl Serialize for DependencyType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DependencyType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(DependencyType::parse(&s))
    }
}

/// A relationship between issues.
///
/// Deser accepts `depends_on_id` (canonical), plus legacy `depends_on`
/// and `target_id` fallbacks — mirrors Go `Dependency.UnmarshalJSON`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    #[serde(default)]
    pub issue_id: String,
    #[serde(default, rename = "depends_on_id")]
    pub depends_on_id: String,
    /// Legacy field name; folded into `depends_on_id` when canonical absent.
    #[serde(
        default,
        rename = "depends_on",
        skip_serializing_if = "String::is_empty"
    )]
    pub depends_on_legacy: String,
    /// Legacy field name; folded into `depends_on_id` when others absent.
    #[serde(
        default,
        rename = "target_id",
        skip_serializing_if = "String::is_empty"
    )]
    pub target_id_legacy: String,
    #[serde(default)]
    pub r#type: DependencyType,
    #[serde(default, serialize_with = "serialize_go_time")]
    pub created_at: Option<String>,
    #[serde(default)]
    pub created_by: String,
}

impl Dependency {
    /// Effective depends-on after legacy-field folding (Go UnmarshalJSON).
    pub fn effective_depends_on(&self) -> &str {
        if !self.depends_on_id.is_empty() {
            &self.depends_on_id
        } else if !self.depends_on_legacy.is_empty() {
            &self.depends_on_legacy
        } else {
            &self.target_id_legacy
        }
    }
}

/// A comment on an issue. `id` tolerates JSON numbers (legacy integer IDs)
/// by stringifying the raw literal verbatim (#145 semantics).
///
/// The serialized name is `issue_id`, matching Go's `Comment.IssueID`
/// (pkg/model/types.go:452). Emitting `issueId` instead made every JSON
/// export carry a key the oracle never writes, so `--export-format json`
/// differed from Go by one key per comment — invisible in a size check
/// (2 bytes per comment) and fatal to any consumer reading the documented
/// field. The camelCase spelling is still accepted on *input*, since that
/// is what some br versions write.
#[derive(Debug, Clone, Serialize)]
pub struct Comment {
    pub id: String,
    pub issue_id: String,
    pub author: String,
    pub text: String,
    pub created_at: Option<String>,
}

impl<'de> Deserialize<'de> for Comment {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default, rename = "id")]
            id: Option<serde_json::Value>,
            #[serde(default, alias = "issueId")]
            issue_id: String,
            #[serde(default)]
            author: String,
            #[serde(default)]
            text: String,
            #[serde(default)]
            created_at: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let id = match raw.id {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(serde_json::Value::String(s)) => s,
            Some(other) => other.to_string().trim().to_string(),
        };
        Ok(Comment {
            id,
            issue_id: raw.issue_id,
            author: raw.author,
            text: raw.text,
            created_at: raw.created_at,
        })
    }
}

/// A trackable work item — 25 fields matching Go `model.Issue` exactly.
///
/// Timestamps stay `Option<String>` at this layer so we can preserve the
/// exact RFC3339(Nano) textual form for data_hash byte-parity; typed
/// parsing happens in the loader with jiff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    #[serde(skip)]
    pub content_hash: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub design: String,
    #[serde(
        default,
        rename = "acceptance_criteria",
        skip_serializing_if = "String::is_empty"
    )]
    pub acceptance_criteria: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    pub status: Status,
    #[serde(default)]
    pub priority: i32,
    #[serde(default, rename = "issue_type")]
    pub issue_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub assignee: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_minutes: Option<i64>,
    #[serde(default, serialize_with = "serialize_go_time")]
    pub created_at: Option<String>,
    #[serde(default, serialize_with = "serialize_go_time")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    /// Scheduler deferral: hidden from ready/actionable until this instant
    /// passes. Go `Issue.DeferUntil` (pkg/model/types.go:30).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_go_time"
    )]
    pub defer_until: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_go_time"
    )]
    pub closed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub compaction_level: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_at_commit: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub original_size: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<Dependency>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<Comment>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_repo: String,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

impl Issue {
    /// Go: `Issue.Validate()` — ID+Title non-empty; timestamps must not be
    /// inverted. Returns human-readable error strings mirroring Go messages.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.id.is_empty() {
            return Err(ValidationError::MissingField("id"));
        }
        if self.title.is_empty() {
            return Err(ValidationError::MissingField("title"));
        }
        // updated_at < created_at rejection (loader feeds these to load_stats)
        if let (Some(c), Some(u)) = (&self.created_at, &self.updated_at) {
            if let (Ok(c), Ok(u)) = (c.parse::<jiff::Timestamp>(), u.parse::<jiff::Timestamp>()) {
                if u < c {
                    return Err(ValidationError::InvertedTimestamps);
                }
            }
        }
        Ok(())
    }
}

/// Sprint — a time-boxed period of work (Go `pkg/model/types.go:Sprint`).
/// Sprints are loaded from `.beads/sprints.jsonl` (one JSON object per line,
/// same JSONL format as issues).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sprint {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bead_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity_target: Option<f64>,
    #[serde(default, serialize_with = "serialize_go_time")]
    pub created_at: Option<String>,
    #[serde(default, serialize_with = "serialize_go_time")]
    pub updated_at: Option<String>,
}

impl Sprint {
    /// True when today falls within start_date..=end_date.
    pub fn is_active(&self) -> bool {
        let (Some(start), Some(end)) = (&self.start_date, &self.end_date) else {
            return false;
        };
        let (Ok(start), Ok(end)) = (
            start.parse::<jiff::Timestamp>(),
            end.parse::<jiff::Timestamp>(),
        ) else {
            return false;
        };
        let now = jiff::Timestamp::now();
        now >= start && now <= end
    }
}

/// Forecast — an ETA prediction for a bead (Go `pkg/model/types.go:Forecast`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Forecast {
    pub bead_id: String,
    pub eta_date: String,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub factors: Vec<String>,
    #[serde(default, serialize_with = "serialize_go_time")]
    pub created_at: Option<String>,
}

/// A single point on a burndown chart (Go `pkg/model/types.go:BurndownPoint`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurndownPoint {
    pub date: String,
    pub remaining: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ideal: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid status: {0}")]
    InvalidStatus(String),
    #[error("invalid dependency type")]
    InvalidDependencyType,
    #[error("updated_at cannot be before created_at")]
    InvertedTimestamps,
}

/// Parse a timestamp string to `jiff::Timestamp`, returning `None` on failure.
pub fn parse_ts(s: &Option<String>) -> Option<jiff::Timestamp> {
    s.as_deref()?.parse::<jiff::Timestamp>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrips_all_ten_values() {
        for raw in [
            "open",
            "in_progress",
            "blocked",
            "deferred",
            "draft",
            "pinned",
            "hooked",
            "review",
            "closed",
            "tombstone",
        ] {
            let s: Status = serde_json::from_value(serde_json::json!(raw)).unwrap();
            assert_eq!(s.as_str(), raw);
        }
    }

    #[test]
    fn dependency_folds_legacy_field_names() {
        // canonical
        let d: Dependency =
            serde_json::from_value(serde_json::json!({"issue_id":"A","depends_on_id":"B"}))
                .unwrap();
        assert_eq!(d.effective_depends_on(), "B");
        // legacy depends_on
        let d: Dependency =
            serde_json::from_value(serde_json::json!({"issue_id":"A","depends_on":"C"})).unwrap();
        assert_eq!(d.effective_depends_on(), "C");
        // legacy target_id
        let d: Dependency =
            serde_json::from_value(serde_json::json!({"issue_id":"A","target_id":"D"})).unwrap();
        assert_eq!(d.effective_depends_on(), "D");
        // canonical wins over legacy
        let d: Dependency = serde_json::from_value(
            serde_json::json!({"issue_id":"A","depends_on_id":"B","target_id":"D"}),
        )
        .unwrap();
        assert_eq!(d.effective_depends_on(), "B");
    }

    #[test]
    fn comment_id_tolerates_numeric_legacy_ids() {
        let c: Comment =
            serde_json::from_value(serde_json::json!({"id": 42, "text": "hi"})).unwrap();
        assert_eq!(c.id, "42");
        let c: Comment =
            serde_json::from_value(serde_json::json!({"id": "uuid-abc", "text": "hi"})).unwrap();
        assert_eq!(c.id, "uuid-abc");
        let c: Comment = serde_json::from_value(serde_json::json!({"text": "no id"})).unwrap();
        assert_eq!(c.id, "");
    }

    #[test]
    fn issue_omits_empty_optionals_like_go() {
        let issue = Issue {
            id: "X-1".into(),
            content_hash: String::new(),
            title: "Test".into(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: Status::Open,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-01-02T00:00:00Z".into()),
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: vec![],
            dependencies: vec![],
            comments: vec![],
            source_repo: String::new(),
        };
        let json = serde_json::to_value(&issue).unwrap();
        let obj = json.as_object().unwrap();
        // omitempty parity
        assert!(!obj.contains_key("design"));
        assert!(!obj.contains_key("labels"));
        assert!(!obj.contains_key("dependencies"));
        assert!(!obj.contains_key("compaction_level")); // zero omitted
        assert!(obj.contains_key("priority")); // no omitempty in Go
        assert_eq!(obj.get("acceptance_criteria"), None);
    }

    #[test]
    fn validate_rejects_inverted_timestamps() {
        let mut issue = minimal_issue();
        issue.created_at = Some("2026-01-05T00:00:00Z".into());
        issue.updated_at = Some("2026-01-01T00:00:00Z".into());
        assert_eq!(issue.validate(), Err(ValidationError::InvertedTimestamps));
    }

    fn minimal_issue() -> Issue {
        Issue {
            id: "X-1".into(),
            content_hash: String::new(),
            title: "T".into(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status: Status::Open,
            priority: 2,
            issue_type: "task".into(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: None,
            updated_at: None,
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: vec![],
            dependencies: vec![],
            comments: vec![],
            source_repo: String::new(),
        }
    }
}

#[cfg(test)]
mod dependency_type_parity_tests {
    use super::DependencyType;

    /// Go `pkg/model/types.go:420-441` defines six dependency types and
    /// treats three of them (plus the legacy empty string) as blocking.
    #[test]
    fn all_six_go_types_round_trip() {
        for raw in [
            "blocks",
            "conditional-blocks",
            "waits-for",
            "related",
            "parent-child",
            "discovered-from",
        ] {
            let parsed = DependencyType::parse(raw);
            assert_eq!(parsed.as_str(), raw, "round trip failed for {raw}");
        }
    }

    #[test]
    fn blocking_set_matches_go() {
        for raw in ["", "blocks", "conditional-blocks", "waits-for"] {
            assert!(
                DependencyType::parse(raw).is_blocking(),
                "Go treats {raw:?} as blocking"
            );
        }
        for raw in [
            "related",
            "parent-child",
            "discovered-from",
            "some-custom-type",
        ] {
            assert!(
                !DependencyType::parse(raw).is_blocking(),
                "Go does not treat {raw:?} as blocking"
            );
        }
    }

    /// The bug this closes: both conditional types previously fell through to
    /// the non-blocking arm, so a `waits-for` edge silently stopped blocking
    /// the issue that depended on it.
    #[test]
    fn conditional_types_are_blocking_not_related() {
        for raw in ["conditional-blocks", "waits-for"] {
            let t = DependencyType::parse(raw);
            assert_ne!(
                t,
                DependencyType::Related,
                "{raw} was collapsing to related"
            );
            assert!(t.is_blocking());
        }
    }

    /// Go `IsValid` is a non-blank test, not an allowlist: custom br
    /// relationships must stay loadable. The loader keeps one deliberate
    /// exception — the legacy empty type, which Go's `IsBlocking` also treats
    /// as blocking and which pre-typed beads data omits entirely.
    #[test]
    fn is_valid_accepts_any_non_blank_type() {
        assert!(DependencyType::raw_is_valid(""));
        assert!(!DependencyType::raw_is_valid("   "));
        assert!(DependencyType::raw_is_valid("blocks"));
        assert!(DependencyType::raw_is_valid("conditional-blocks"));
        assert!(DependencyType::raw_is_valid("waits-for"));
        assert!(DependencyType::raw_is_valid("tracks-instead-of-blocks"));
    }

    /// Regression guard: the loader rejects an issue whose dependency type
    /// fails `raw_is_valid`, so an over-strict predicate silently drops whole
    /// issues instead of failing loudly.
    #[test]
    fn every_go_typed_dependency_loads() {
        for raw in [
            "blocks",
            "conditional-blocks",
            "waits-for",
            "related",
            "parent-child",
            "discovered-from",
            "",
        ] {
            assert!(
                DependencyType::raw_is_valid(raw),
                "loader would reject an issue carrying a {raw:?} dependency"
            );
        }
    }

    /// Unknown types stay non-blocking, matching Go's `IsBlocking` for a
    /// custom relationship.
    #[test]
    fn unknown_types_are_non_blocking() {
        let t = DependencyType::parse("tracks-instead-of-blocks");
        assert!(!t.is_blocking());
    }

    /// The legacy empty type still blocks, via the `Blocks` default.
    #[test]
    fn empty_type_defaults_to_blocks() {
        assert_eq!(DependencyType::parse(""), DependencyType::Blocks);
        assert!(DependencyType::parse("").is_blocking());
    }
}
