//! Discovery chain + GitLoader tests.
use std::path::Path;
use std::sync::Mutex;

// Env vars are process-global; serialize every test that touches them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn discovers_fixture_beads_dir() {
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/small_chain"
    );
    let dir = bv_core::discovery::get_beads_dir(fixture.as_ref()).expect("discovery works");
    assert!(dir.ends_with(".beads"), "resolved to {}", dir.display());
    let jsonl = bv_core::discovery::find_jsonl_path_with_warnings(&dir, |_| {}).unwrap();
    assert!(jsonl.unwrap().file_name().unwrap() == "issues.jsonl");
}

#[test]
fn beads_db_env_takes_priority() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir().join(format!("bvr-disc-{}", std::process::id()));
    std::fs::create_dir_all(tmp.join(".beads")).unwrap();
    std::env::set_var("BEADS_DIR", tmp.join(".beads"));
    let dir = bv_core::discovery::get_beads_dir(Path::new("/")).unwrap();
    std::env::remove_var("BEADS_DIR");
    assert_eq!(dir, tmp.join(".beads"));
}

#[test]
fn redirect_follow_resolves_target() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir().join(format!("bvr-redir-{}", std::process::id()));
    let src = tmp.join("_beads");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(tmp.join(".beads")).unwrap();
    std::fs::write(tmp.join(".beads/redirect"), src.display().to_string()).unwrap();
    // No BEADS_* env interference
    std::env::remove_var("BEADS_DIR");
    std::env::remove_var("BEADS_DB");
    let resolved = bv_core::discovery::get_beads_dir(&tmp).unwrap();
    assert_eq!(resolved, src);
}

#[test]
fn redirect_loop_is_error() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir().join(format!("bvr-loop-{}", std::process::id()));
    std::fs::create_dir_all(tmp.join(".beads")).unwrap();
    std::fs::write(
        tmp.join(".beads/redirect"),
        tmp.join(".beads").display().to_string(),
    )
    .unwrap();
    std::env::remove_var("BEADS_DIR");
    std::env::remove_var("BEADS_DB");
    let result = bv_core::discovery::get_beads_dir(&tmp);
    assert!(result.is_err(), "loop must error, not silently fall back");
}

#[test]
fn git_loader_resolves_head_and_loads_selfrepo() {
    let repo = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let loader = bv_core::discovery::GitLoader::new(repo);
    let sha = loader.resolve_revision("HEAD").expect("HEAD resolves");
    assert_eq!(sha.len(), 40);
    let _ = loader.load_at("HEAD").expect("load_at HEAD works");
}

/// Phase B acceptance (TUI_UX_PARITY_PLAN.md): the exact pipeline the TUI
/// Time-Travel view and `--robot-diff` share — `git show <rev>` parsed
/// through the tolerant loader, then `diff_issues` — against a known git
/// history fixture built hermetically in a temp dir (no dependence on the
/// outer repo's own history).
#[test]
fn git_loader_load_at_sees_committed_snapshot_diff() {
    use std::process::Command;
    if Command::new("git").arg("--version").output().is_err() {
        return; // no git binary — nothing to exercise
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("bvr-tt-{}-{n}", std::process::id()));
    let beads = tmp.join(".beads");
    std::fs::create_dir_all(&beads).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&tmp)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let commit = |msg: &str| {
        git(&["add", ".beads/issues.jsonl"]);
        git(&[
            "-c",
            "user.email=bvr-test@example.com",
            "-c",
            "user.name=bvr-test",
            "commit",
            "-qm",
            msg,
        ]);
    };
    // Rev 1: issue A open. `issue_type` is required — Go `Issue.Validate`
    // rejects a record without one.
    std::fs::write(
        beads.join("issues.jsonl"),
        "{\"id\":\"A\",\"title\":\"alpha\",\"status\":\"open\",\"issue_type\":\"task\"}\n",
    )
    .unwrap();
    git(&["init", "-q"]);
    commit("one");
    // Rev 2 (== HEAD): A closed, B new.
    std::fs::write(
        beads.join("issues.jsonl"),
        "{\"id\":\"A\",\"title\":\"alpha\",\"status\":\"closed\",\"issue_type\":\"task\"}\n{\"id\":\"B\",\"title\":\"beta\",\"status\":\"open\",\"issue_type\":\"task\"}\n",
    )
    .unwrap();
    commit("two");

    let loader = bv_core::discovery::GitLoader::new(&tmp);
    let at_prev = loader.load_at("HEAD~1").expect("HEAD~1 loads");
    assert_eq!(at_prev.len(), 1, "rev 1 has exactly issue A");
    assert_eq!(at_prev[0].id, "A");
    assert!(
        matches!(at_prev[0].status, bv_core::model::Status::Open),
        "rev 1 must show A open"
    );
    let at_head = loader.load_at("HEAD").expect("HEAD loads");
    assert_eq!(at_head.len(), 2);
    // The diff half (`diff_issues` over these snapshots) is covered on the
    // consumer side (bv-tui `time_travel_apply_*` tests); this gate pins
    // the git-show plumbing feeding it: HEAD shows A closed + B new.
    assert!(at_head
        .iter()
        .any(|i| i.id == "A" && matches!(i.status, bv_core::model::Status::Closed)));
    assert!(at_head.iter().any(|i| i.id == "B"));

    assert!(
        loader
            .load_at("refs/heads/definitely-not-a-bvr-test-ref")
            .is_err(),
        "unknown ref must error (robot maps this to exit 1)"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
