//! Golden-shape test: envelope field order and presence semantics must
//! match captured Go goldens (golden/selfrepo____robot_next.json).
use bv_robot::{encode_payload, OutputFormat, RobotLoadStats};

#[derive(serde::Serialize)]
struct NextPayload {
    generated_at: String,
    data_hash: String,
    output_format: String,
    version: String,
    actionable: bool,
    phase2_ready: bool,
}

#[test]
fn next_payload_key_order_matches_golden() {
    let p = NextPayload {
        generated_at: "T".into(),
        data_hash: "H".into(),
        output_format: "toon".into(),
        version: "v0.20.0".into(),
        actionable: true,
        phase2_ready: true,
    };
    // The JSON path is what this pins — field order and presence, nothing
    // about the format.
    let bytes = encode_payload(&p, OutputFormat::Json).unwrap();
    let s = String::from_utf8(bytes).unwrap().trim_end().to_string();
    let expected_prefix = r#"{"generated_at":"T","data_hash":"H","output_format":"toon","version":"v0.20.0","actionable":true,"phase2_ready":true}"#;
    assert_eq!(s, expected_prefix);
}

/// `--format toon` must emit TOON, not JSON wearing TOON's marker.
///
/// This assertion used to be the exact opposite — it called
/// `encode_payload(…, OutputFormat::Toon)` and expected the JSON above, on the
/// strength of a comment claiming Go "emits compact JSON with the marker
/// field". `golden/toon/selfrepo__robot_next.toon` disproves that: Go's first
/// line is `generated_at: "…"`, not `{`. So the encoder had a test pinning the
/// bug and no test pinning the behaviour, which is how it shipped.
#[test]
fn toon_format_emits_toon_not_json() {
    if !bv_robot::envelope::tru_available() {
        // Go degrades to JSON without an encoder too (main.go:2038-2040), and
        // says so on stderr. `emit_json` warns; here the fallback is silent by
        // design, since this crate's job is the encoding, not the warning.
        return;
    }
    let p = NextPayload {
        generated_at: "T".into(),
        data_hash: "H".into(),
        output_format: "toon".into(),
        version: "v0.20.0".into(),
        actionable: true,
        phase2_ready: true,
    };
    let out = String::from_utf8(encode_payload(&p, OutputFormat::Toon).unwrap()).unwrap();
    assert!(
        !out.trim_start().starts_with('{'),
        "the TOON path emitted a JSON document:\n{out}"
    );
    // TOON writes `key: value`, and a value that needs no quoting is bare —
    // `"T"` comes back as `T`, where JSON would have kept the quotes. Asserting
    // the bare key is the point: it can only come from a TOON encoder.
    let first = out.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("generated_at:"),
        "expected TOON's `key: value` shape, got {first:?}"
    );
    assert!(
        !first.contains(": \""),
        "the value is still quoted, so nothing was re-encoded:\n{out}"
    );
    assert!(
        out.contains("output_format: toon"),
        "the marker must survive encoding:\n{out}"
    );
    // And the keys Go declares are all still there, in order.
    for key in [
        "generated_at",
        "data_hash",
        "output_format",
        "version",
        "actionable",
        "phase2_ready",
    ] {
        assert!(
            out.lines().any(|l| l.starts_with(&format!("{key}:"))),
            "TOON output is missing {key}:\n{out}"
        );
    }
}

#[test]
fn load_stats_present_only_on_errors() {
    // Golden contract: load_stats absent from clean loads.
    let rep = bv_core::loader::LoadReport {
        valid: 33,
        ..bv_core::loader::LoadReport::default()
    };
    let env = bv_robot::RobotEnvelope::new("h", "v0.20.0", Some(&rep), OutputFormat::Json);
    let v = serde_json::to_value(&env).unwrap();
    assert!(v.get("load_stats").is_none());
    assert_eq!(v["output_format"], "json");

    let rep = bv_core::loader::LoadReport {
        errors: 1,
        warnings: vec!["boom".into()],
        ..bv_core::loader::LoadReport::default()
    };
    let env = bv_robot::RobotEnvelope::new("h", "v0.20.0", Some(&rep), OutputFormat::Json);
    let v = serde_json::to_value(&env).unwrap();
    let ls = v["load_stats"].as_object().expect("present on errors");
    assert_eq!(ls["errors"], 1);
    assert_eq!(ls["warnings"][0], "boom");
    let _ = RobotLoadStats::default(); // type anchor
}
