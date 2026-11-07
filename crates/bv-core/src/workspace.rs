//! Multi-repo workspace support — port of Go `pkg/workspace/types.go`.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub repos: Vec<RepoConfig>,
    #[serde(default)]
    pub discovery: Option<DiscoveryConfig>,
    #[serde(default)]
    pub defaults: Option<Defaults>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    #[serde(default)]
    pub name: Option<String>,
    pub path: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default, rename = "beads_path")]
    pub beads_path: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_patterns")]
    pub patterns: Vec<String>,
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
}

fn default_patterns() -> Vec<String> {
    [
        "*",
        "packages/*",
        "apps/*",
        "services/*",
        "libs/*",
        "modules/*",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}
fn default_exclude() -> Vec<String> {
    ["node_modules", "vendor", ".git", "dist", "build", "target"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}
fn default_max_depth() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default, rename = "beads_path")]
    pub beads_path: Option<String>,
}

/// Find workspace config by walking up from `dir` to root.
///
/// Go `workspace.FindWorkspaceConfig` (`pkg/workspace/types.go:194-220`): a bare
/// upward walk for `.bv/workspace.yaml`, with no `.beads` gate. The gate lives
/// one level up in [`discover_workspace_config`], exactly as it does in Go.
pub fn find_workspace_config(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    loop {
        let candidate = current.join(".bv").join("workspace.yaml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Pick a workspace config for auto-discovery, honouring the `.beads` gate.
///
/// Go `discoverWorkspaceConfig` (`cmd/bv/main.go:9969-9981`): resolve the
/// tracker dir first — `GetBeadsDir("")` walks BEADS_DB → BEADS_DIR →
/// `<cwd>/.beads` → the worktree's main-repo `.beads`
/// (`pkg/loader/loader.go:202-243`) — and give up on workspace mode the moment
/// that resolves to a real directory. The `is_dir` check is load-bearing
/// because `GetBeadsDir` returns the unresolved path when nothing is there
/// (`pkg/loader/loader.go:240-243`). Only when no `.beads` is reachable does
/// the upward walk run. A present `.beads` always wins, so a nested single repo
/// inside a workspace keeps its own view unless `--workspace` overrides it
/// (`cmd/bv/main.go:2624-2631`).
pub fn discover_workspace_config(dir: &Path) -> Option<PathBuf> {
    if let Ok(beads_dir) = crate::discovery::get_beads_dir(dir) {
        if beads_dir.is_dir() {
            return None;
        }
    }
    find_workspace_config(dir)
}

/// Load and validate a workspace config.
pub fn load_workspace(path: &Path) -> Result<WorkspaceConfig, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let config: WorkspaceConfig =
        serde_yaml_ng::from_str(&raw).map_err(|e| format!("parsing {}: {e}", path.display()))?;

    if config.repos.is_empty() && config.discovery.as_ref().is_some_and(|d| !d.enabled) {
        return Err("workspace requires at least 1 repo or discovery.enabled".into());
    }
    // Check for duplicate prefixes (effective, like Go Validate → GetPrefix)
    let mut prefixes = std::collections::HashSet::new();
    for repo in &config.repos {
        if repo.path.is_empty() {
            return Err("repo.path is required".into());
        }
        let prefix = repo.get_prefix().to_lowercase();
        if !prefixes.insert(prefix.clone()) {
            return Err(format!("duplicate prefix: {prefix}"));
        }
    }
    Ok(config)
}

/// Qualify an issue ID with the repo prefix (idempotent).
///
/// Go `QualifyID` (`pkg/workspace/types.go:296-304`): an empty local id or an
/// empty prefix is returned untouched, an already-prefixed id is left alone, and
/// otherwise the prefix is concatenated with no inserted separator. The
/// already-prefixed test is a raw string prefix, not a `prefix + separator`
/// match — so prefix `"a"` leaves `"apple"` alone. Prefixes are conventionally
/// stored *with* their trailing separator, which is why `source_repo_key_from_prefix`
/// strips it again for the `SourceRepo` field.
pub fn qualify_id(prefix: &str, local_id: &str) -> String {
    if local_id.is_empty() || prefix.is_empty() {
        return local_id.to_string();
    }
    if local_id.starts_with(prefix) {
        local_id.to_string()
    } else {
        format!("{prefix}{local_id}")
    }
}

/// Go `sourceRepoKeyFromPrefix` (`pkg/workspace/loader.go:557-561`): the
/// `SourceRepo` value a namespaced issue carries. Trim whitespace, strip every
/// trailing `-`, `:` and `_`, then lowercase. This is also the key
/// `filterByRepo`'s `SourceRepo` fallback matches against, so it is stored
/// separator-free on purpose.
pub fn source_repo_key_from_prefix(prefix: &str) -> String {
    prefix
        .trim()
        .trim_end_matches(['-', ':', '_'])
        .to_lowercase()
}

/// Go `hasKnownPrefix` (`pkg/workspace/loader.go:604-612`): strictly longer than
/// the prefix, raw prefix compare. Used to decide whether a cross-repo
/// dependency reference is already qualified and must be left byte-identical.
fn has_known_prefix(id: &str, known_prefixes: &HashSet<String>) -> bool {
    known_prefixes
        .iter()
        .any(|prefix| id.len() > prefix.len() && id.starts_with(prefix.as_str()))
}

/// Result of loading one repo in a workspace.
#[derive(Debug, Clone)]
pub struct LoadResult {
    pub repo_name: String,
    pub issue_count: usize,
    pub error: Option<String>,
}

impl RepoConfig {
    /// Effective display name (Go GetName).
    pub fn get_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            std::path::Path::new(&self.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.path.clone())
        })
    }

    /// Effective ID prefix (Go GetPrefix): prefix, else lowercased name + "-".
    pub fn get_prefix(&self) -> String {
        if let Some(p) = &self.prefix {
            return p.clone();
        }
        format!("{}-", self.get_name().to_lowercase())
    }

    /// Effective beads dir relative to repo (Go GetBeadsPath).
    pub fn get_beads_path(&self) -> &str {
        self.beads_path.as_deref().unwrap_or(".beads")
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

/// Discover repos under root using the config's discovery settings (Go discoverRepos).
pub fn discover_repos(config: &WorkspaceConfig, root: &Path) -> Vec<RepoConfig> {
    let Some(disc) = &config.discovery else {
        return Vec::new();
    };
    if !disc.enabled {
        return Vec::new();
    }
    let mut found: Vec<RepoConfig> = Vec::new();
    for pattern in &disc.patterns {
        // Walk the pattern path segment by segment (e.g. "packages/*" →
        // root/packages/<name> containing .beads).
        let mut candidates = vec![root.to_path_buf()];
        for (depth, seg) in pattern.split('/').enumerate() {
            if depth >= disc.max_depth {
                break;
            }
            let mut next = Vec::new();
            for base in &candidates {
                let Ok(entries) = std::fs::read_dir(base) else {
                    continue;
                };
                let mut names: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect();
                names.sort();
                for name in names {
                    if disc.exclude.iter().any(|x| x == &name) || name.starts_with('.') {
                        continue;
                    }
                    if seg == "*" || name == *seg {
                        next.push(base.join(&name));
                    }
                }
            }
            candidates = next;
        }
        for c in candidates {
            if !c.join(".beads").is_dir() {
                continue;
            }
            let rel = c
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if rel.is_empty() || found.iter().any(|r| r.path == rel) {
                continue;
            }
            found.push(RepoConfig {
                name: None,
                path: rel,
                prefix: None,
                beads_path: None,
                enabled: Some(true),
            });
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// Identity key for repo dedupe in [`load_all`]: the canonical path of
/// `root.join(repo.path)`, falling back to the joined path when the directory
/// does not exist (canonicalize fails) so literal duplicates still match.
fn repo_key(root: &Path, repo: &RepoConfig) -> PathBuf {
    let joined = root.join(&repo.path);
    std::fs::canonicalize(&joined).unwrap_or(joined)
}

/// Load all enabled repos and merge issues with prefixed IDs (Go AggregateLoader.LoadAll).
/// Returns merged issues plus per-repo results.
pub fn load_all(
    config: &WorkspaceConfig,
    root: &Path,
) -> Result<(Vec<crate::model::Issue>, Vec<LoadResult>), String> {
    let mut repos: Vec<RepoConfig> = config
        .repos
        .iter()
        .filter(|r| r.is_enabled())
        .cloned()
        .collect();
    // Dedupe discovered repos against the configured ones by canonical path:
    // a repo listed in `repos:` that discovery also finds would otherwise be
    // loaded twice, duplicating every issue and inflating counts. Canonical
    // paths absorb `./x`, `x/`, symlinked and case-variant spellings of the
    // same directory; a repo that does not exist on disk falls back to its
    // joined path so exact duplicates still collapse.
    let mut seen: HashSet<PathBuf> = repos.iter().map(|r| repo_key(root, r)).collect();
    for discovered in discover_repos(config, root) {
        if seen.insert(repo_key(root, &discovered)) {
            repos.push(discovered);
        }
    }
    if repos.is_empty() {
        return Err("no enabled repositories in workspace".into());
    }

    let mut all_issues = Vec::new();
    let mut results = Vec::new();
    let mut failures = 0usize;

    // Every enabled repo's prefix, used to recognise an already-qualified
    // cross-repo dependency reference (Go `knownPrefixes`).
    let known_prefixes: HashSet<String> = repos.iter().map(|r| r.get_prefix()).collect();

    for repo in &repos {
        let repo_path = root.join(&repo.path);
        let name = repo.get_name();
        match crate::discovery::load_issues_from_repo(&repo_path) {
            Ok((mut issues, _stats)) => {
                let prefix = repo.get_prefix();
                // Captured before any qualifying, so the membership test below
                // sees this repo's own bare ids (Go `localIDs`).
                let local_ids: HashSet<String> = issues.iter().map(|i| i.id.clone()).collect();
                let source_repo = source_repo_key_from_prefix(&prefix);
                for issue in &mut issues {
                    issue.id = qualify_id(&prefix, &issue.id);
                    issue.source_repo = source_repo.clone();
                    for dep in &mut issue.dependencies {
                        // The owning issue is namespaced too, not just the target.
                        dep.issue_id = qualify_id(&prefix, &dep.issue_id);
                        let target = dep.effective_depends_on().to_string();
                        // A reference that already carries another repo's prefix
                        // is external and stays byte-identical; everything else
                        // is either local or assumed local.
                        if !has_known_prefix(&target, &known_prefixes)
                            || local_ids.contains(&target)
                        {
                            dep.depends_on_id = qualify_id(&prefix, &target);
                        }
                    }
                    for comment in &mut issue.comments {
                        comment.issue_id = qualify_id(&prefix, &comment.issue_id);
                    }
                }
                results.push(LoadResult {
                    repo_name: name,
                    issue_count: issues.len(),
                    error: None,
                });
                all_issues.extend(issues);
            }
            Err(e) => {
                failures += 1;
                results.push(LoadResult {
                    repo_name: name,
                    issue_count: 0,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    if failures == results.len() {
        return Err(format!(
            "all {failures} enabled repositories failed to load"
        ));
    }
    Ok((all_issues, results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualify_id_is_idempotent() {
        assert_eq!(qualify_id("api-", "AUTH-1"), "api-AUTH-1");
        assert_eq!(qualify_id("api-", "api-AUTH-1"), "api-AUTH-1");
    }

    #[test]
    fn qualify_id_leaves_empty_sides_untouched() {
        // Go QualifyID returns localID unchanged when either side is empty. The
        // empty-local case is the one Rust got wrong by prefixing it: an empty
        // id is a dangling reference, not an id that happens to need a prefix.
        assert_eq!(qualify_id("api-", ""), "");
        assert_eq!(qualify_id("", "AUTH-1"), "AUTH-1");
        assert_eq!(qualify_id("", ""), "");
    }

    #[test]
    fn source_repo_key_strips_every_trailing_separator() {
        // Go TrimRight takes a cutset, so this strips a run, not one character.
        assert_eq!(source_repo_key_from_prefix("api-"), "api");
        assert_eq!(source_repo_key_from_prefix("api:"), "api");
        assert_eq!(source_repo_key_from_prefix("api_"), "api");
        assert_eq!(source_repo_key_from_prefix("api-:-_"), "api");
        assert_eq!(source_repo_key_from_prefix("  API-  "), "api");
        assert_eq!(source_repo_key_from_prefix("."), ".");
    }

    #[test]
    fn known_prefix_detection_requires_strictly_longer() {
        let known: HashSet<String> = ["api-".to_string(), "web-".to_string()].into();
        assert!(has_known_prefix("web-9", &known));
        assert!(has_known_prefix("api-deep", &known));
        // Exactly the prefix is not "known" — it is a local bare id.
        assert!(!has_known_prefix("api-", &known));
        // A different separator is a different prefix, not a near miss.
        assert!(!has_known_prefix("web:9", &known));
        assert!(!has_known_prefix("other-1", &known));
    }

    #[test]
    fn parse_workspace_yaml() {
        let yaml = r#"
repos:
  - name: api
    path: services/api
  - name: web
    path: apps/web
    prefix: web-
"#;
        let config: WorkspaceConfig = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(config.repos.len(), 2);
        assert_eq!(config.repos[0].path, "services/api");
    }
}

#[cfg(test)]
mod discover_tests {
    use super::*;

    /// The gate consults the full tracker-dir chain (Go
    /// `loader.GetBeadsDir("")`, `pkg/loader/loader.go:202-243`), so these
    /// fixtures only mean something when BEADS_DB/BEADS_DIR are unset.
    fn tracker_dir_env_is_default() -> bool {
        std::env::var("BEADS_DB").is_err() && std::env::var("BEADS_DIR").is_err()
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bvr_ws_discover_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".bv")).unwrap();
        std::fs::write(dir.join(".bv").join("workspace.yaml"), "repos: []\n").unwrap();
        dir
    }

    #[test]
    fn discovers_workspace_when_no_beads_dir_is_reachable() {
        if !tracker_dir_env_is_default() {
            return;
        }
        let root = scratch("no_beads");
        let found = discover_workspace_config(&root);
        assert_eq!(found, Some(root.join(".bv").join("workspace.yaml")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovers_workspace_from_a_nested_directory() {
        if !tracker_dir_env_is_default() {
            return;
        }
        let root = scratch("nested");
        let nested = root.join("services").join("api");
        std::fs::create_dir_all(&nested).unwrap();
        let found = discover_workspace_config(&nested);
        assert_eq!(found, Some(root.join(".bv").join("workspace.yaml")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_present_beads_dir_wins_over_a_workspace_config() {
        // Go `discoverWorkspaceConfig` returns "" (main.go:9971-9974) as soon as
        // `GetBeadsDir("")` resolves to a real directory, so a repo holding both
        // `.beads/` and `.bv/workspace.yaml` stays single-repo. The bare walk-up
        // still finds the config, which is why the two are separate functions.
        if !tracker_dir_env_is_default() {
            return;
        }
        let root = scratch("both");
        std::fs::create_dir_all(root.join(".beads")).unwrap();
        assert_eq!(discover_workspace_config(&root), None);
        assert_eq!(
            find_workspace_config(&root),
            Some(root.join(".bv").join("workspace.yaml"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_beads_file_is_not_a_beads_dir() {
        // The gate is `info.IsDir()` (main.go:9972), not mere existence: a
        // `.beads` that is a file must not shadow the workspace.
        if !tracker_dir_env_is_default() {
            return;
        }
        let root = scratch("beads_file");
        std::fs::write(root.join(".beads"), "not a directory").unwrap();
        let found = discover_workspace_config(&root);
        assert_eq!(found, Some(root.join(".bv").join("workspace.yaml")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_workspace_config_stays_none() {
        if !tracker_dir_env_is_default() {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("bvr_ws_discover_empty_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(discover_workspace_config(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod aggregate_tests {
    use super::*;

    #[test]
    fn load_all_prefixes_ids_and_merges() {
        let dir = std::env::temp_dir().join(format!("bvr_ws_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for repo in ["api", "web"] {
            let beads = dir.join(repo).join(".beads");
            std::fs::create_dir_all(&beads).unwrap();
            std::fs::write(
                beads.join("issues.jsonl"),
                format!(
                    "{{\"id\":\"{repo}-1\",\"title\":\"T1\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"dependencies\":[]}}\n"
                ),
            )
            .unwrap();
        }
        let config = WorkspaceConfig {
            name: Some("test".into()),
            repos: vec![
                RepoConfig {
                    name: None,
                    path: "api".into(),
                    prefix: None,
                    beads_path: None,
                    enabled: None,
                },
                RepoConfig {
                    name: None,
                    path: "web".into(),
                    prefix: None,
                    beads_path: None,
                    enabled: None,
                },
            ],
            discovery: None,
            defaults: None,
        };
        let (issues, results) = load_all(&config, &dir).expect("load_all");
        assert_eq!(issues.len(), 2);
        assert!(issues
            .iter()
            .any(|i| i.id == "api-1" && i.source_repo == "api"));
        assert!(issues
            .iter()
            .any(|i| i.id == "web-1" && i.source_repo == "web"));
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.error.is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_all_namespaces_dep_owner_comments_and_keeps_external_refs() {
        let dir = std::env::temp_dir().join(format!("bvr_ws_ns_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for repo in ["api", "web"] {
            let beads = dir.join(repo).join(".beads");
            std::fs::create_dir_all(&beads).unwrap();
        }
        // One api issue that owns a local dependency, references another repo,
        // and carries a comment. Every reference is deliberately left bare in
        // the source so the namespacer has something to do to each of them.
        std::fs::write(
            dir.join("api").join(".beads").join("issues.jsonl"),
            concat!(
                r#"{"id":"A-1","title":"T1","status":"open","priority":1,"issue_type":"task","#,
                r#""dependencies":["#,
                r#"{"issue_id":"A-1","depends_on_id":"A-2","type":"blocks"},"#,
                r#"{"issue_id":"A-1","depends_on_id":"web-7","type":"blocks"}],"#,
                r#""comments":[{"issueId":"A-1","text":"note"}]}"#,
                "\n",
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("web").join(".beads").join("issues.jsonl"),
            "{\"id\":\"W-1\",\"title\":\"T2\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"dependencies\":[]}\n",
        )
        .unwrap();

        // A display name distinct from the prefix: source_repo must come from
        // the prefix (separator-stripped, lowercased), not the repo name.
        let config = WorkspaceConfig {
            name: None,
            repos: vec![
                RepoConfig {
                    name: Some("ApiService".into()),
                    path: "api".into(),
                    prefix: Some("api-".into()),
                    beads_path: None,
                    enabled: None,
                },
                RepoConfig {
                    name: Some("WebApp".into()),
                    path: "web".into(),
                    prefix: Some("web-".into()),
                    beads_path: None,
                    enabled: None,
                },
            ],
            discovery: None,
            defaults: None,
        };
        let (issues, _) = load_all(&config, &dir).expect("load_all");
        let a = issues
            .iter()
            .find(|i| i.id == "api-A-1")
            .expect("namespaced issue");

        assert_eq!(a.source_repo, "api", "from the prefix, not the name");
        assert_eq!(a.dependencies.len(), 2);

        // The owning issue is namespaced on the dependency too — this field
        // feeds compute_data_hash, so leaving it bare changed the hash.
        assert_eq!(a.dependencies[0].issue_id, "api-A-1");
        // A local target is qualified.
        assert_eq!(a.dependencies[0].depends_on_id, "api-A-2");
        // A target already carrying another repo's known prefix is external and
        // must stay byte-identical.
        assert_eq!(a.dependencies[1].depends_on_id, "web-7");
        assert_eq!(a.dependencies[1].issue_id, "api-A-1");

        assert_eq!(a.comments.len(), 1);
        assert_eq!(a.comments[0].issue_id, "api-A-1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_all_fails_when_all_repos_fail() {
        let dir = std::env::temp_dir().join(format!("bvr_ws_fail_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = WorkspaceConfig {
            name: None,
            repos: vec![RepoConfig {
                name: None,
                path: "nope".into(),
                prefix: None,
                beads_path: None,
                enabled: None,
            }],
            discovery: None,
            defaults: None,
        };
        assert!(load_all(&config, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repo listed in `repos:` that discovery also finds must load once:
    /// before dedupe it contributed its issues twice.
    #[test]
    fn load_all_dedupes_config_repo_against_discovery() {
        let dir = std::env::temp_dir().join(format!("bvr_ws_dedupe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let beads = dir.join("api").join(".beads");
        std::fs::create_dir_all(&beads).unwrap();
        std::fs::write(
            beads.join("issues.jsonl"),
            "{\"id\":\"A-1\",\"title\":\"T\",\"status\":\"open\",\"priority\":1,\"issue_type\":\"task\",\"dependencies\":[]}\n",
        )
        .unwrap();
        let config = WorkspaceConfig {
            name: None,
            repos: vec![RepoConfig {
                name: None,
                // `./api` vs discovered `api` — same canonical directory.
                path: "./api".into(),
                prefix: None,
                beads_path: None,
                enabled: None,
            }],
            discovery: Some(DiscoveryConfig {
                enabled: true,
                patterns: default_patterns(),
                exclude: default_exclude(),
                max_depth: 2,
            }),
            defaults: None,
        };
        let (issues, results) = load_all(&config, &dir).expect("load_all");
        assert_eq!(issues.len(), 1, "repo must not load twice: {issues:?}");
        assert_eq!(results.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_finds_beads_dirs() {
        let dir = std::env::temp_dir().join(format!("bvr_ws_disc_{}", std::process::id()));
        for name in ["api", "packages/web", "node_modules/skip"] {
            let beads = dir.join(name).join(".beads");
            std::fs::create_dir_all(&beads).unwrap();
        }
        let config = WorkspaceConfig {
            name: None,
            repos: vec![],
            discovery: Some(DiscoveryConfig {
                enabled: true,
                patterns: default_patterns(),
                exclude: default_exclude(),
                max_depth: 2,
            }),
            defaults: None,
        };
        let found = discover_repos(&config, &dir);
        let paths: Vec<&str> = found.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"api"));
        assert!(paths.contains(&"packages/web"));
        assert!(!paths.iter().any(|p| p.contains("node_modules")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
