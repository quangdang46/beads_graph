//! Edge case tests ported from Go pkg/loader tests.

use bv_core::loader::{parse_issues_with_options, ParseOptions, ParseStats};
use bv_core::model::{Issue, Status};

fn parse(input: &str) -> (Vec<Issue>, ParseStats, Vec<String>) {
    let mut warnings = Vec::new();
    let mut rdr = input.as_bytes();
    let (issues, stats) = parse_issues_with_options(&mut rdr, &ParseOptions::default(), |w| {
        warnings.push(w.to_string())
    })
    .unwrap();
    (issues, stats, warnings)
}

// === Go: TestLoadIssuesRobustness ===
#[test]
fn garbage_lines_skipped_not_crash() {
    let input = r#"not json at all
{"id":"OK-1","title":"Valid","status":"open","issue_type":"task"}
{broken json here
{"no_id_field":"missing","title":"No ID"}
{"id":"OK-2","title":"Also valid","status":"closed","issue_type":"bug"}"#;
    let (issues, stats, _) = parse(input);
    assert_eq!(issues.len(), 2);
    assert_eq!(stats.errors, 3); // malformed + no ID + broken
}

// === Go: BOM handling ===
#[test]
fn bom_only_on_first_line() {
    let input = "\u{feff}{\"id\":\"A\",\"title\":\"T\",\"status\":\"open\",\"issue_type\":\"task\"}\n{\"id\":\"B\",\"title\":\"T2\",\"status\":\"open\",\"issue_type\":\"task\"}";
    let (issues, _, _) = parse(input);
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].id, "A");
}

// === Go: CRLF line endings ===
#[test]
fn crlf_line_endings_handled() {
    let input = "{\"id\":\"A\",\"title\":\"T\",\"status\":\"open\",\"issue_type\":\"task\"}\r\n{\"id\":\"B\",\"title\":\"T2\",\"status\":\"open\",\"issue_type\":\"task\"}\r\n";
    let (issues, _, _) = parse(input);
    assert_eq!(issues.len(), 2);
}

// === Go: empty lines skipped silently ===
#[test]
fn empty_lines_between_records() {
    let input = "\n\n{\"id\":\"A\",\"title\":\"T\",\"status\":\"open\",\"issue_type\":\"task\"}\n\n\n{\"id\":\"B\",\"title\":\"T2\",\"status\":\"open\",\"issue_type\":\"task\"}\n";
    let (issues, stats, _) = parse(input);
    assert_eq!(issues.len(), 2);
    assert_eq!(stats.valid, 2);
    assert_eq!(stats.errors, 0);
}

// === Go: _type dispatch — memory/sprint/forecast/burndown silently skipped ===
#[test]
fn type_memory_silently_skipped() {
    let (issues, stats, warnings) = parse(r#"{"_type":"memory","content":"remember this"}"#);
    assert!(issues.is_empty());
    assert_eq!(stats.skipped, 1);
    assert!(warnings.is_empty());
}

#[test]
fn type_sprint_forecast_burndown_all_skipped() {
    let input = r#"{"_type":"sprint"}
{"_type":"forecast"}
{"_type":"burndown"}
{"_type":"memory"}"#;
    let (_, stats, warnings) = parse(input);
    assert_eq!(stats.skipped, 4);
    assert!(warnings.is_empty());
}

#[test]
fn type_unknown_also_skipped_silently() {
    let (_, stats, _) = parse(r#"{"_type":"something_new"}"#);
    assert_eq!(stats.skipped, 1);
}

#[test]
fn type_issue_explicitly_parsed() {
    let (issues, stats, _) =
        parse(r#"{"_type":"issue","id":"A","title":"T","status":"open","issue_type":"task"}"#);
    assert_eq!(issues.len(), 1);
    assert_eq!(stats.valid, 1);
}

#[test]
fn missing_type_defaults_to_issue() {
    let (issues, _, _) = parse(r#"{"id":"A","title":"T","status":"open","issue_type":"task"}"#);
    assert_eq!(issues.len(), 1);
}

// === Go: updated_at < created_at rejection ===
#[test]
fn inverted_timestamps_rejected() {
    let (_, stats, warnings) = parse(
        r#"{"id":"A","title":"T","status":"open","issue_type":"task","created_at":"2026-01-10T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    );
    assert_eq!(stats.errors, 1);
    assert_eq!(stats.valid, 0);
    assert!(warnings[0].contains("invalid issue"));
}

#[test]
fn equal_timestamps_accepted() {
    let (issues, stats, _) = parse(
        r#"{"id":"A","title":"T","status":"open","issue_type":"task","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}"#,
    );
    assert_eq!(issues.len(), 1);
    assert_eq!(stats.valid, 1);
}

// === Go: status normalization (trim + lowercase) ===
#[test]
fn status_uppercase_normalized() {
    let (issues, _, _) = parse(r#"{"id":"A","title":"T","status":"OPEN","issue_type":"task"}"#);
    assert_eq!(issues[0].status, Status::Open);
}

#[test]
fn status_with_spaces_trimmed() {
    let (issues, _, _) =
        parse(r#"{"id":"A","title":"T","status":"  closed  ","issue_type":"task"}"#);
    assert_eq!(issues[0].status, Status::Closed);
}

#[test]
fn status_invalid_rejected() {
    let (_, stats, _) = parse(r#"{"id":"A","title":"T","status":"banana","issue_type":"task"}"#);
    assert_eq!(stats.errors, 1);
    assert_eq!(stats.valid, 0);
}

// === Go: Dependency field aliases ===
#[test]
fn dependency_depends_on_id_canonical() {
    let (issues, _, _) = parse(
        r#"{"id":"A","title":"T","status":"open","issue_type":"task","dependencies":[{"depends_on_id":"B"}]}"#,
    );
    assert_eq!(issues[0].dependencies[0].effective_depends_on(), "B");
}

#[test]
fn dependency_depends_on_legacy() {
    let (issues, _, _) = parse(
        r#"{"id":"A","title":"T","status":"open","issue_type":"task","dependencies":[{"depends_on":"C"}]}"#,
    );
    assert_eq!(issues[0].dependencies[0].effective_depends_on(), "C");
}

#[test]
fn dependency_target_id_legacy() {
    let (issues, _, _) = parse(
        r#"{"id":"A","title":"T","status":"open","issue_type":"task","dependencies":[{"target_id":"D"}]}"#,
    );
    assert_eq!(issues[0].dependencies[0].effective_depends_on(), "D");
}

// === Go: Comment numeric ID tolerance ===
#[test]
fn comment_numeric_id_stringified() {
    let (issues, _, _) = parse(
        r#"{"id":"A","title":"T","status":"open","issue_type":"task","comments":[{"id":42,"text":"hello"}]}"#,
    );
    assert_eq!(issues[0].comments[0].id, "42");
}

#[test]
fn comment_uuid_id_preserved() {
    let (issues, _, _) = parse(
        r#"{"id":"A","title":"T","status":"open","issue_type":"task","comments":[{"id":"550e8400-e29b-41d4-a716-446655440000","text":"hi"}]}"#,
    );
    assert_eq!(
        issues[0].comments[0].id,
        "550e8400-e29b-41d4-a716-446655440000"
    );
}

// === Go: dep.issue_id backfilled from parent ===
#[test]
fn dependency_issue_id_backfilled() {
    let (issues, _, _) = parse(
        r#"{"id":"PARENT","title":"T","status":"open","issue_type":"task","dependencies":[{"depends_on_id":"CHILD"}]}"#,
    );
    assert_eq!(issues[0].dependencies[0].issue_id, "PARENT");
}

// === Go: over-long lines skipped with warning ===
#[test]
fn very_long_line_skipped() {
    let big_id = "X".repeat(500);
    let input = format!(
        "{{\"id\":\"{}\",\"title\":\"T\",\"status\":\"open\",\"issue_type\":\"task\"}}",
        big_id
    );
    let mut warnings = Vec::new();
    let opts = ParseOptions {
        buffer_size: Some(100),
    };
    let mut rdr = input.as_bytes();
    let (issues, _stats) =
        parse_issues_with_options(&mut rdr, &opts, |w| warnings.push(w.to_string())).unwrap();
    assert!(issues.is_empty());
    assert!(warnings.iter().any(|w| w.contains("line too long")));
}

// === Go: empty file → 0 issues, 0 errors ===
#[test]
fn empty_file_no_errors() {
    let (issues, stats, _) = parse("");
    assert!(issues.is_empty());
    assert_eq!(stats.valid, 0);
    assert_eq!(stats.errors, 0);
}

// === Go: tombstone filtered after load ===
///
/// Go `pkg/workspace/loader.go:519-527` walks the parsed records and routes
/// `status: "tombstone"` into `TombstoneIDs`, so a deleted issue never reaches
/// a view, a count, or the `data_hash`. The split runs *after* parsing, which is
/// why `valid` still counts the record and only `visible` excludes it.
#[test]
fn tombstones_are_kept_out_of_the_issue_set() {
    let (issues, stats, _) = parse(concat!(
        r#"{"id":"LIVE","title":"Real","status":"open","issue_type":"task"}"#,
        "\n",
        r#"{"id":"DEAD","title":"Gone","status":"tombstone","issue_type":"task"}"#,
    ));

    let ids: Vec<&str> = issues.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["LIVE"], "the deleted record must not be analysable");
    assert!(!issues.iter().any(|i| i.status == Status::Tombstone));

    assert_eq!(stats.tombstone_ids, ["DEAD"]);
    assert_eq!(
        stats.valid, 2,
        "a tombstone is a well-formed record, not a parse error — Go's split runs after parsing"
    );
    assert_eq!(stats.visible(), 1);
    assert_eq!(stats.errors, 0, "dropping a tombstone is not an error");
}
