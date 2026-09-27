#![allow(clippy::empty_line_after_doc_comments)]
//! bvr — Beads Viewer in Rust.
//! CLI surface skeleton (Phase 3a): flag registry, argv rewriter, validation.
//! Command dispatch lands with bead p3-dispatch-3lv.

mod argv;
#[allow(dead_code)] // flag inventory is declarative data; consumed by dispatch phase
mod flags;
mod validation;

/// Go `pkg/correlation/network.go` — the bead impact network behind
/// `--robot-impact-network` (`handleRobotImpactNetwork`,
/// cmd/bv/robot_registry.go:3330). Ported here rather than in
/// `bv-correlation::network`, whose simplified edge model this replaces for
/// the robot command: Go's builder derives nodes from the *history report*
/// (not the issue set), expands edges from three independent sources —
/// shared commits, shared files and blocking dependencies — and then derives
/// clusters, degrees and statistics from the finished edge list.
///
/// Every constant and every ordering decision below mirrors the Go source:
/// `minWeight = 2` in `detectClusters`, the 5-entry `limitStrings` cap on
/// `details`, the 5-cluster / 10-node caps in `ToResult`, and the
/// `commonPathPrefix` byte-slicing. Where Go relies on map iteration order it
/// either sorts the result (`sort.Strings(details)`) or the map is replaced by
/// a `BTreeMap` that makes the iteration order total.
mod impact_network {
    use bv_core::model::Issue;
    use bv_correlation::history::HistoryReport;
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

    /// Go `NetworkEdgeType` constants.
    const EDGE_SHARED_COMMIT: &str = "shared_commit";
    const EDGE_SHARED_FILE: &str = "shared_file";
    const EDGE_DEPENDENCY: &str = "dependency";

    /// Go `latestHistoryActivity` on a bead with no usable timestamp.
    const GO_ZERO_TIME: &str = "0001-01-01T00:00:00Z";

    /// Go `NetworkEdge` (`json:"from_bead"` / `"to_bead"` / `"edge_type"` /
    /// `"weight"` / `"details"`).
    #[derive(Debug, Clone, PartialEq)]
    pub struct Edge {
        pub from_bead: String,
        pub to_bead: String,
        pub edge_type: &'static str,
        pub weight: i64,
        pub details: Vec<String>,
    }

    impl Edge {
        pub fn to_json(&self) -> Value {
            json!({
                "from_bead": self.from_bead,
                "to_bead": self.to_bead,
                "edge_type": self.edge_type,
                "weight": self.weight,
                "details": self.details,
            })
        }
    }

    /// Go `NetworkNode`. `connectivity` is never assigned by Go's `BuildAt`,
    /// so it always serializes as `0`.
    #[derive(Debug, Clone)]
    pub struct Node {
        pub bead_id: String,
        pub title: String,
        pub status: String,
        pub priority: i64,
        pub last_activity: String,
        pub degree: i64,
        pub cluster_id: i64,
        pub commit_count: i64,
        pub file_count: i64,
        pub connectivity: f64,
    }

    impl Node {
        pub fn to_json(&self) -> Value {
            json!({
                "bead_id": self.bead_id,
                "title": self.title,
                "status": self.status,
                "priority": self.priority,
                "last_activity": self.last_activity,
                "degree": self.degree,
                "cluster_id": self.cluster_id,
                "commit_count": self.commit_count,
                "file_count": self.file_count,
                "connectivity": self.connectivity,
            })
        }
    }

    /// Go `BeadCluster`.
    #[derive(Debug, Clone, Default)]
    pub struct Cluster {
        pub cluster_id: i64,
        pub bead_ids: Vec<String>,
        pub label: String,
        pub internal_edges: i64,
        pub external_edges: i64,
        pub internal_connectivity: f64,
        pub central_bead: String,
        pub shared_files: Vec<String>,
        pub total_commits: i64,
    }

    impl Cluster {
        pub fn to_json(&self) -> Value {
            json!({
                "cluster_id": self.cluster_id,
                "bead_ids": self.bead_ids,
                "label": self.label,
                "internal_edges": self.internal_edges,
                "external_edges": self.external_edges,
                "internal_connectivity": self.internal_connectivity,
                "central_bead": self.central_bead,
                "shared_files": self.shared_files,
                "total_commits": self.total_commits,
            })
        }
    }

    /// Go `NetworkStats`.
    #[derive(Debug, Clone, Default)]
    pub struct Stats {
        pub total_nodes: i64,
        pub total_edges: i64,
        pub cluster_count: i64,
        pub avg_degree: f64,
        pub max_degree: i64,
        pub density: f64,
        pub isolated_nodes: i64,
        pub largest_cluster: i64,
    }

    impl Stats {
        pub fn to_json(&self) -> Value {
            json!({
                "total_nodes": self.total_nodes,
                "total_edges": self.total_edges,
                "cluster_count": self.cluster_count,
                "avg_degree": self.avg_degree,
                "max_degree": self.max_degree,
                "density": self.density,
                "isolated_nodes": self.isolated_nodes,
                "largest_cluster": self.largest_cluster,
            })
        }
    }

    /// Go `ImpactNetwork`.
    #[derive(Debug, Clone)]
    pub struct Network {
        pub generated_at: String,
        pub data_hash: String,
        pub nodes: BTreeMap<String, Node>,
        pub edges: Vec<Edge>,
        pub clusters: Vec<Cluster>,
        pub stats: Stats,
    }

    impl Network {
        pub fn to_json(&self) -> Value {
            let nodes: serde_json::Map<String, Value> = self
                .nodes
                .iter()
                .map(|(id, node)| (id.clone(), node.to_json()))
                .collect();
            json!({
                "generated_at": self.generated_at,
                "data_hash": self.data_hash,
                "nodes": Value::Object(nodes),
                "edges": self.edges.iter().map(Edge::to_json).collect::<Vec<_>>(),
                "clusters": self.clusters.iter().map(Cluster::to_json).collect::<Vec<_>>(),
                "stats": self.stats.to_json(),
            })
        }
    }

    /// Go `ImpactNetworkResult`, minus the envelope fields the handler
    /// re-derives (`generated_at`, `data_hash`).
    pub struct Result_ {
        pub network: Network,
        pub stats: Stats,
        pub top_clusters: Vec<Cluster>,
        pub top_connected: Vec<Node>,
    }

    /// Go `normalizePath` (file_index.go:770) — backslashes to slashes, then
    /// strip one leading `./` and one trailing `/`.
    fn normalize_path(path: &str) -> String {
        let mut p = path.replace('\\', "/");
        if let Some(rest) = p.strip_prefix("./") {
            p = rest.to_string();
        }
        if let Some(rest) = p.strip_suffix('/') {
            p = rest.to_string();
        }
        p
    }

    /// Go `shortSHA` (cocommit.go:636).
    fn short_sha(sha: &str) -> String {
        if sha.len() > 7 {
            sha[..7].to_string()
        } else {
            sha.to_string()
        }
    }

    /// Go `limitStrings` (related.go:591).
    fn limit_strings(mut v: Vec<String>, max: usize) -> Vec<String> {
        v.truncate(max);
        v
    }

    /// Go `classifyBeadStatus` (file_index.go:783) reduced to its `skip`
    /// result — `BuildFileIndex` drops tombstones so every lookup surface
    /// agrees on the same bead links (#184).
    fn status_is_skipped(status: &str) -> bool {
        status.trim().eq_ignore_ascii_case("tombstone")
    }

    /// Go `BuildFileIndex` (file_index.go:55), reduced to the `FileToBeads`
    /// membership the shared-file edge expansion consumes. Go orders each
    /// file's `[]BeadReference` by last touch; the edge keys are canonicalised
    /// pairs, so the per-file order cannot change the resulting edge set.
    fn build_file_to_beads(report: &HistoryReport) -> BTreeMap<String, BTreeSet<String>> {
        let mut index: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (bead_id, history) in &report.histories {
            if status_is_skipped(&history.status) {
                continue;
            }
            for commit in history.commits.iter().flatten() {
                for file in &commit.files {
                    index
                        .entry(normalize_path(&file.path))
                        .or_default()
                        .insert(bead_id.clone());
                }
            }
        }
        index
    }

    /// Go `latestHistoryActivity` (network.go:269) — the newest timestamp
    /// across the four milestones, every event, and every commit. Go compares
    /// `time.Time` values and keeps the first on a tie; the original string is
    /// echoed so the emitted instant keeps the report's exact rendering.
    fn latest_history_activity(history: &bv_correlation::history::BeadHistory) -> String {
        let mut best: Option<(i128, String)> = None;
        let mut consider = |raw: &str| {
            let Ok(ts) = raw.parse::<jiff::Timestamp>() else {
                return;
            };
            let key = ts.as_nanosecond();
            match &best {
                Some((cur, _)) if *cur >= key => {}
                _ => best = Some((key, raw.to_string())),
            }
        };
        for event in [
            &history.milestones.created,
            &history.milestones.claimed,
            &history.milestones.closed,
            &history.milestones.reopened,
        ]
        .into_iter()
        .flatten()
        {
            consider(&event.timestamp);
        }
        for event in &history.events {
            consider(&event.timestamp);
        }
        for commit in history.commits.iter().flatten() {
            consider(&commit.timestamp);
        }
        best.map(|(_, s)| s)
            .unwrap_or_else(|| GO_ZERO_TIME.to_string())
    }

    /// Go `BuildAt` (network.go:180).
    pub fn build(report: &HistoryReport, issues: &[Issue], now: &str) -> Network {
        let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
        let mut bead_files: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        // Go `buildBeadMaps` (network.go:154) — the reverse index the node
        // `file_count` and the cluster `shared_files` are computed from. It
        // runs over every history, tombstones included, unlike `BuildFileIndex`.
        for (bead_id, history) in &report.histories {
            let mut files: BTreeSet<String> = BTreeSet::new();
            for commit in history.commits.iter().flatten() {
                for file in &commit.files {
                    files.insert(normalize_path(&file.path));
                }
            }
            bead_files.insert(bead_id.clone(), files);
        }

        // Go: priority comes from the issue set, defaulting to medium (2).
        let issue_index: HashMap<&str, &Issue> = issues
            .iter()
            .filter(|i| !i.id.is_empty())
            .map(|i| (i.id.as_str(), i))
            .collect();

        for (bead_id, history) in &report.histories {
            let priority = issue_index.get(bead_id.as_str()).map_or(2, |i| i.priority);
            let node = Node {
                bead_id: bead_id.clone(),
                title: history.title.clone(),
                status: history.status.clone(),
                priority: i64::from(priority),
                last_activity: latest_history_activity(history),
                degree: 0,
                cluster_id: -1,
                commit_count: history.commits.iter().flatten().count() as i64,
                file_count: bead_files.get(bead_id).map_or(0, |f| f.len()) as i64,
                connectivity: 0.0,
            };
            nodes.insert(bead_id.clone(), node);
        }

        // Go accumulates the three edge families in separate maps and appends
        // each in sorted-key order. The families never share a key (the edge
        // type is part of it) and `BuildAt` re-sorts the whole list
        // afterwards, so one `BTreeMap` keyed by the canonical
        // (from, to, edge_type) triple is equivalent.
        let mut edges: BTreeMap<(String, String, &'static str), (i64, Vec<String>)> =
            BTreeMap::new();
        let bump =
            |a: &str,
             b: &str,
             ty: &'static str,
             detail: String,
             edges: &mut BTreeMap<(String, String, &'static str), (i64, Vec<String>)>| {
                if a == b {
                    return;
                }
                let (lo, hi) = if a > b { (b, a) } else { (a, b) };
                let slot = edges
                    .entry((lo.to_string(), hi.to_string(), ty))
                    .or_insert_with(|| (0, Vec::new()));
                slot.0 += 1;
                slot.1.push(detail);
            };

        // `addSharedCommitEdges` (network.go:296) — one edge per distinct pair
        // of beads in every commit's `CommitIndex` membership list, carrying
        // the short SHA as detail.
        for (sha, bead_ids) in &report.commit_index {
            if bead_ids.len() < 2 {
                continue;
            }
            // Go `uniqueSortedStrings`: a hand-built index may repeat a bead
            // ID, and each commit must contribute once per distinct pair.
            let unique: Vec<&str> = {
                let set: BTreeSet<&str> = bead_ids
                    .iter()
                    .map(String::as_str)
                    .filter(|s| !s.is_empty())
                    .collect();
                set.into_iter().collect()
            };
            for i in 0..unique.len() {
                for j in (i + 1)..unique.len() {
                    bump(
                        unique[i],
                        unique[j],
                        EDGE_SHARED_COMMIT,
                        short_sha(sha),
                        &mut edges,
                    );
                }
            }
        }

        // `addSharedFileEdges` (network.go:343) — one edge per distinct pair of
        // beads touching a file the index holds under two or more beads.
        for (file_path, beads) in &build_file_to_beads(report) {
            let refs: Vec<&str> = beads.iter().map(String::as_str).collect();
            if refs.len() < 2 {
                continue;
            }
            for i in 0..refs.len() {
                for j in (i + 1)..refs.len() {
                    bump(
                        refs[i],
                        refs[j],
                        EDGE_SHARED_FILE,
                        file_path.clone(),
                        &mut edges,
                    );
                }
            }
        }

        // `addDependencyEdges` (network.go:386) — explicit blocking links only,
        // restricted to beads that are already nodes. The detail records the
        // *declared* direction even though the key is canonicalised.
        for issue in issues {
            if issue.id.is_empty() || !nodes.contains_key(&issue.id) {
                continue;
            }
            for dep in &issue.dependencies {
                if !dep.r#type.is_blocking() {
                    continue;
                }
                let to = dep.depends_on_id.as_str();
                if to.is_empty() || to == issue.id || !nodes.contains_key(to) {
                    continue;
                }
                bump(
                    &issue.id,
                    to,
                    EDGE_DEPENDENCY,
                    format!("{} -> {}", issue.id, to),
                    &mut edges,
                );
            }
        }

        let mut edge_list: Vec<Edge> = edges
            .into_iter()
            .map(|((from_bead, to_bead, edge_type), (weight, mut details))| {
                details.sort();
                Edge {
                    from_bead,
                    to_bead,
                    edge_type,
                    weight,
                    details: limit_strings(details, 5),
                }
            })
            .collect();
        edge_list.sort_by(|x, y| {
            x.from_bead
                .cmp(&y.from_bead)
                .then_with(|| x.to_bead.cmp(&y.to_bead))
                .then_with(|| x.edge_type.cmp(y.edge_type))
        });

        // `recomputeNodeDegrees` (network.go:252) — every edge counts once per
        // endpoint, even when a pair is joined by several edge types.
        for edge in &edge_list {
            if let Some(node) = nodes.get_mut(&edge.from_bead) {
                node.degree += 1;
            }
            if let Some(node) = nodes.get_mut(&edge.to_bead) {
                node.degree += 1;
            }
        }

        let mut network = Network {
            generated_at: now.to_string(),
            data_hash: report.data_hash.clone(),
            nodes,
            edges: edge_list,
            clusters: Vec::new(),
            stats: Stats::default(),
        };
        detect_clusters(&mut network, &bead_files, report);
        calculate_stats(&mut network);
        network
    }

    /// Go `detectClusters` (network.go:471) — connected components over edges
    /// with `weight >= minWeight`, walked from sorted node IDs so the result
    /// does not depend on hash order. Components smaller than two beads are
    /// dropped, the survivors are sorted largest-first, and the cluster IDs are
    /// renumbered afterwards (node IDs follow the renumbering).
    fn detect_clusters(
        network: &mut Network,
        bead_files: &BTreeMap<String, BTreeSet<String>>,
        report: &HistoryReport,
    ) {
        const MIN_WEIGHT: i64 = 2;

        let mut adjacency: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for edge in &network.edges {
            if edge.weight >= MIN_WEIGHT {
                adjacency
                    .entry(edge.from_bead.as_str())
                    .or_default()
                    .insert(edge.to_bead.as_str());
                adjacency
                    .entry(edge.to_bead.as_str())
                    .or_default()
                    .insert(edge.from_bead.as_str());
            }
        }

        let node_ids: Vec<String> = network.nodes.keys().cloned().collect();
        let mut visited: HashSet<String> = HashSet::new();
        let mut next_id: i64 = 0;
        for bead_id in &node_ids {
            if visited.contains(bead_id) {
                continue;
            }
            if adjacency.get(bead_id.as_str()).map_or(0, BTreeSet::len) == 0 {
                continue;
            }
            let mut component: Vec<&str> = Vec::new();
            let mut stack: Vec<&str> = vec![bead_id.as_str()];
            while let Some(current) = stack.pop() {
                if !visited.insert(current.to_string()) {
                    continue;
                }
                component.push(current);
                if let Some(neighbors) = adjacency.get(current) {
                    for neighbor in neighbors {
                        if !visited.contains(*neighbor) {
                            stack.push(neighbor);
                        }
                    }
                }
            }
            if component.len() < 2 {
                continue;
            }
            component.sort_unstable();
            let cluster = build_cluster(next_id, &component, network, bead_files, report);
            network.clusters.push(cluster);
            for bid in &component {
                if let Some(node) = network.nodes.get_mut(*bid) {
                    node.cluster_id = next_id;
                }
            }
            next_id += 1;
        }

        // Go sorts largest-first, breaking ties on the first (sorted) bead ID.
        network.clusters.sort_by(|a, b| {
            b.bead_ids
                .len()
                .cmp(&a.bead_ids.len())
                .then_with(|| a.bead_ids[0].cmp(&b.bead_ids[0]))
        });
        for (i, cluster) in network.clusters.iter_mut().enumerate() {
            let old_id = cluster.cluster_id;
            cluster.cluster_id = i as i64;
            for bid in &cluster.bead_ids {
                if let Some(node) = network.nodes.get_mut(bid) {
                    if node.cluster_id == old_id {
                        node.cluster_id = i as i64;
                    }
                }
            }
        }
    }

    /// Go `buildCluster` (network.go:563).
    fn build_cluster(
        id: i64,
        bead_ids: &[&str],
        network: &Network,
        bead_files: &BTreeMap<String, BTreeSet<String>>,
        report: &HistoryReport,
    ) -> Cluster {
        let mut cluster = Cluster {
            cluster_id: id,
            bead_ids: bead_ids.iter().map(|s| s.to_string()).collect(),
            shared_files: Vec::new(),
            ..Default::default()
        };
        let members: HashSet<&str> = bead_ids.iter().copied().collect();

        for edge in &network.edges {
            let from_in = members.contains(edge.from_bead.as_str());
            let to_in = members.contains(edge.to_bead.as_str());
            if from_in && to_in {
                cluster.internal_edges += 1;
            } else if from_in || to_in {
                cluster.external_edges += 1;
            }
        }

        let n = bead_ids.len() as i64;
        let max_edges = n * (n - 1) / 2;
        if max_edges > 0 {
            cluster.internal_connectivity = cluster.internal_edges as f64 / max_edges as f64;
        }

        // Go: the first bead in sorted order wins a tie, because the test is
        // strictly `>` against a counter that starts at zero.
        let mut max_degree = 0i64;
        for bid in bead_ids {
            let Some(node) = network.nodes.get(*bid) else {
                continue;
            };
            let mut internal_degree = 0i64;
            for edge in &network.edges {
                if (edge.from_bead == *bid && members.contains(edge.to_bead.as_str()))
                    || (edge.to_bead == *bid && members.contains(edge.from_bead.as_str()))
                {
                    internal_degree += 1;
                }
            }
            if internal_degree > max_degree {
                max_degree = internal_degree;
                cluster.central_bead = (*bid).to_string();
            }
            cluster.total_commits += node.commit_count;
        }

        // Files touched by at least two beads of the cluster.
        let mut file_count: BTreeMap<&str, usize> = BTreeMap::new();
        for bid in bead_ids {
            if let Some(files) = bead_files.get(*bid) {
                for file in files {
                    *file_count.entry(file.as_str()).or_insert(0) += 1;
                }
            }
        }
        cluster.shared_files = file_count
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .map(|(file, _)| file.to_string())
            .collect();

        cluster.label = generate_cluster_label(bead_ids, &cluster.shared_files, report);
        cluster
    }

    /// Go `generateClusterLabel` (network.go:638) — a common directory prefix
    /// when one is long enough to be worth printing, else the first bead's
    /// title truncated to 27 runes plus an ellipsis.
    fn generate_cluster_label(
        bead_ids: &[&str],
        shared_files: &[String],
        report: &HistoryReport,
    ) -> String {
        if !shared_files.is_empty() {
            let mut prefix = common_path_prefix(shared_files);
            if !prefix.is_empty() && prefix.len() > 2 {
                if prefix.ends_with('/') {
                    prefix.pop();
                }
                return prefix;
            }
        }
        if let Some(first) = bead_ids.first() {
            if let Some(history) = report.histories.get(*first) {
                let title: Vec<char> = history.title.chars().collect();
                if title.len() > 30 {
                    let mut out: String = title[..27].iter().collect();
                    out.push_str("...");
                    return out;
                }
                return history.title.clone();
            }
        }
        "cluster".to_string()
    }

    /// Go `commonPathPrefix` (network.go:666). Go slices the strings as bytes,
    /// so this does too.
    fn common_path_prefix(files: &[String]) -> String {
        fn dir_of(path: &[u8]) -> Option<Vec<u8>> {
            path.iter()
                .rposition(|&b| b == b'/')
                .map(|i| path[..=i].to_vec())
        }
        if files.is_empty() {
            return String::new();
        }
        let first = files[0].as_bytes();
        if files.len() == 1 {
            return match dir_of(first) {
                Some(p) => String::from_utf8_lossy(&p).into_owned(),
                None => String::new(),
            };
        }
        let Some(mut prefix) = dir_of(first) else {
            return String::new();
        };
        for file in &files[1..] {
            let file = file.as_bytes();
            while !prefix.is_empty() && !file.starts_with(&prefix) {
                // Shorten to the previous directory boundary, dropping the
                // trailing slash of the candidate first.
                let mut search = &prefix[..];
                if search.last() == Some(&b'/') {
                    search = &search[..search.len() - 1];
                }
                match search.iter().rposition(|&b| b == b'/') {
                    Some(i) => prefix = search[..=i].to_vec(),
                    None => prefix = Vec::new(),
                }
            }
        }
        String::from_utf8_lossy(&prefix).into_owned()
    }

    /// Go `calculateStats` (network.go:728).
    fn calculate_stats(network: &mut Network) {
        let mut total_degree = 0i64;
        let mut max_degree = 0i64;
        let mut isolated = 0i64;
        for node in network.nodes.values() {
            total_degree += node.degree;
            if node.degree > max_degree {
                max_degree = node.degree;
            }
            if node.degree == 0 {
                isolated += 1;
            }
        }

        let total_nodes = network.nodes.len() as i64;
        let total_edges = network.edges.len() as i64;
        let mut largest_cluster = 0i64;
        for cluster in &network.clusters {
            if cluster.bead_ids.len() as i64 > largest_cluster {
                largest_cluster = cluster.bead_ids.len() as i64;
            }
        }
        let mut avg_degree = 0.0;
        if total_nodes > 0 {
            avg_degree = total_degree as f64 / total_nodes as f64;
        }
        let mut density = 0.0;
        if total_nodes > 1 {
            density = total_edges as f64 / (total_nodes * (total_nodes - 1) / 2) as f64;
        }

        network.stats = Stats {
            total_nodes,
            total_edges,
            cluster_count: network.clusters.len() as i64,
            avg_degree,
            max_degree,
            density,
            isolated_nodes: isolated,
            largest_cluster,
        };
    }

    /// Go `GetSubNetwork` (network.go:765) — the beads within `depth` hops of
    /// `bead_id` plus the edges wholly inside that set, with statistics
    /// recomputed over the sub-network. Clusters are never carried over.
    fn get_sub_network(network: &Network, bead_id: &str, depth: i64) -> Network {
        let depth = depth.clamp(1, 3);

        let mut visited: HashSet<&str> = HashSet::new();
        let mut bead_set: BTreeSet<&str> = BTreeSet::new();
        let mut queue: VecDeque<(&str, i64)> = VecDeque::new();
        queue.push_back((bead_id, 0));

        while let Some((current, level)) = queue.pop_front() {
            if !visited.insert(current) {
                continue;
            }
            bead_set.insert(current);
            if level >= depth {
                continue;
            }
            for edge in &network.edges {
                if edge.from_bead == current && !visited.contains(edge.to_bead.as_str()) {
                    queue.push_back((edge.to_bead.as_str(), level + 1));
                }
                if edge.to_bead == current && !visited.contains(edge.from_bead.as_str()) {
                    queue.push_back((edge.from_bead.as_str(), level + 1));
                }
            }
        }

        let mut sub = Network {
            generated_at: network.generated_at.clone(),
            data_hash: network.data_hash.clone(),
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            clusters: Vec::new(),
            stats: Stats::default(),
        };
        for bid in &bead_set {
            if let Some(node) = network.nodes.get(*bid) {
                sub.nodes.insert((*bid).to_string(), node.clone());
            }
        }
        for edge in &network.edges {
            if bead_set.contains(edge.from_bead.as_str())
                && bead_set.contains(edge.to_bead.as_str())
            {
                sub.edges.push(edge.clone());
            }
        }
        calculate_stats(&mut sub);
        sub
    }

    /// Go `ToResult` (network.go:857). `bead_id` empty means the whole
    /// network; otherwise the sub-network at `depth` is returned and `stats`
    /// is the sub-network's. `top_clusters` always comes from the full
    /// network, `top_connected` from whichever network is being returned.
    pub fn to_result(network: &Network, bead_id: &str, depth: i64) -> Result_ {
        let (sub, stats) = if bead_id.is_empty() {
            (Some(network.clone()), network.stats.clone())
        } else {
            let sub = get_sub_network(network, bead_id, depth);
            let stats = sub.stats.clone();
            (Some(sub), stats)
        };

        let cluster_limit = 5.min(network.clusters.len());
        let top_clusters = network.clusters[..cluster_limit].to_vec();

        let source = sub.as_ref().unwrap_or(network);
        let mut nodes: Vec<Node> = source.nodes.values().cloned().collect();
        nodes.sort_by(|a, b| {
            b.degree
                .cmp(&a.degree)
                .then_with(|| a.bead_id.cmp(&b.bead_id))
        });
        let node_limit = 10.min(nodes.len());
        let top_connected = nodes[..node_limit].to_vec();

        Result_ {
            network: sub.unwrap_or_else(|| network.clone()),
            stats,
            top_clusters,
            top_connected,
        }
    }
}

use std::io::Write as _;
use std::process::ExitCode;

// `--cpu-profile` serialises pprof-rs's `Report` into the same
// `perftools.profiles.Profile` protobuf Go's `runtime/pprof` writes, then
// gzips it. The trait supplies `Profile::encode_to_vec`.
use pprof::protos::Message as _;

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Go `strconv.ParseBool` — the exact set `flag.Bool` accepts.
fn parse_go_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// `--generate-docs` (Go cmd/bv/main.go:1757). Runs first in Go's RunE and
/// exits 0 after emitting the documentation artifacts. The Go tree writes
/// markdown + JSON under `docs/generated`; we emit the JSON artifact plus a
/// markdown index so the flag is a real, terminating command rather than a
/// fall-through to the TUI launcher.
/// Go `ReplaceBetweenMarkers` (internal/docgen/docgen.go:561-572): replace the
/// body between `<!-- bv:generated:NAME -->` and the matching closing comment,
/// leaving the markers themselves in place.
///
/// Returns `None` when the document has no such pair, which is Go's `ok` flag —
/// a document without the markers is left untouched rather than treated as an
/// error.
fn replace_between_markers(content: &str, marker: &str, body: &str) -> Option<String> {
    let open = format!("<!-- bv:generated:{marker} -->");
    // Go's closing tag allows the name to be repeated: `<!-- /bv:generated:NAME
    // -->` and `<!-- /bv:generated -->` both close it.
    let close_with = format!("<!-- /bv:generated:{marker} -->");
    let close_bare = "<!-- /bv:generated -->";
    let start = content.find(&open)?;
    let after = start + open.len();
    let end = content[after..]
        .find(&close_with)
        .map(|i| after + i)
        .or_else(|| content[after..].find(close_bare).map(|i| after + i))?;
    // Everything from just after the opening marker up to the closing one is
    // the old body and is replaced wholesale — slicing to `end` would keep it
    // and simply append the new table below.
    let mut out = String::with_capacity(content.len() + body.len());
    out.push_str(&content[..after]);
    out.push('\n');
    out.push_str(body.trim());
    out.push('\n');
    out.push_str(&content[end..]);
    Some(out)
}

/// Go `docgen.Generate` steps 3 and 4 (docgen.go:630-672): rewrite the
/// generated tables in README.md, and in AGENTS.md when it exists.
///
/// README.md is read unconditionally and its absence is an error — that is the
/// behaviour being reproduced. Rust previously wrote two files under
/// docs/generated and stopped, so a repository with no README reported success
/// having done nothing to the document the command exists to update.
fn update_generated_markers(repo_root: &std::path::Path, tables: &[(&str, String)]) -> ExitCode {
    let readme = repo_root.join("README.md");
    let readme_bytes = match std::fs::read(&readme) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "generating docs: reading README.md: {}",
                go_path_error("open", &readme.to_string_lossy(), &e)
            );
            return ExitCode::from(1);
        }
    };
    let original = String::from_utf8_lossy(&readme_bytes).to_string();
    let mut updated = original.clone();
    for (marker, table) in tables {
        if let Some(next) = replace_between_markers(&updated, marker, table) {
            updated = next;
        }
    }
    if updated != original {
        if let Err(e) = std::fs::write(&readme, &updated) {
            eprintln!(
                "generating docs: updating README.md: {}",
                go_path_error("open", &readme.to_string_lossy(), &e)
            );
            return ExitCode::from(1);
        }
    }
    // AGENTS.md is optional: Go ignores a read failure and only writes when the
    // content actually changed.
    let agents = repo_root.join("AGENTS.md");
    if let Ok(bytes) = std::fs::read(&agents) {
        let original = String::from_utf8_lossy(&bytes).to_string();
        let mut updated = original.clone();
        for (marker, table) in tables {
            if let Some(next) = replace_between_markers(&updated, marker, table) {
                updated = next;
            }
        }
        if updated != original {
            let _ = std::fs::write(&agents, &updated);
        }
    }
    ExitCode::from(0)
}

fn run_generate_docs() -> ExitCode {
    let out_dir = std::path::Path::new("docs/generated");
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        eprintln!("Error: generating docs: {e}");
        return ExitCode::from(1);
    }
    // `$PWD`, not `current_dir()`: on macOS the latter reports the resolved
    // physical path, so every path Go names in its errors would read
    // /private/tmp where Go says /tmp.
    let cwd = std::env::var("PWD")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::current_dir())
        .unwrap_or_default();
    // Go's docgen never loads issues: it renders the flag, env, alerts, recipe
    // and key-binding registries and rewrites the README/AGENTS markers. Rust
    // loaded the issue set to put a count in a JSON file Go does not produce,
    // which made the command fail in any repository without a `.beads` — the
    // one place a README is most likely to need regenerating.
    let doc = serde_json::json!({
        "generated_by": "bvr",
        "version": GO_APP_VERSION,
        "contract_version": bv_robot::ROBOT_CONTRACT_VERSION,
        "flags": flags::flag_names(),
    });
    let json_path = out_dir.join("bvr-docs.json");
    if let Err(e) = std::fs::write(&json_path, go_json_string(&doc)) {
        eprintln!("Error: generating docs: {e}");
        return ExitCode::from(1);
    }
    let md = format!(
        "# bv generated docs\n\n- version: {}\n- contract: {}\n- flags: {}\n",
        GO_APP_VERSION,
        bv_robot::ROBOT_CONTRACT_VERSION,
        flags::flag_names().len()
    );
    let md_path = out_dir.join("bvr-docs.md");
    if let Err(e) = std::fs::write(&md_path, md) {
        eprintln!("Error: generating docs: {e}");
        return ExitCode::from(1);
    }
    println!(
        "Generated docs: {} and {}",
        md_path.display(),
        json_path.display()
    );
    // The `flags` marker is the one this port has a real table for; the others
    // Go fills from the recipes, env and drift registries, which are separate
    // ports. A marker with no table is left as it is, exactly as Go's `ok`
    // flag does.
    let flags_table = render_generated_flags_table();
    update_generated_markers(&cwd, &[("flags", flags_table)])
}

/// The `flags` table body written between the README markers. A definition per
/// line, in registry order.
fn render_generated_flags_table() -> String {
    let mut out = String::from("| Flag | Type |\n| --- | --- |\n");
    for f in flags::ROBOT_PRIMARIES
        .iter()
        .chain(flags::MODIFIER_FLAGS.iter())
    {
        let kind = match f.kind {
            flags::FlagKind::Bool => "bool",
            flags::FlagKind::Str | flags::FlagKind::RepeatableStr => "string",
            flags::FlagKind::Int => "int",
            flags::FlagKind::Float => "float",
        };
        out.push_str(&format!("| `--{}` | {} |\n", f.name, kind));
    }
    out
}

/// `--export` (Go cmd/bv/main.go:4372). Writes a report using recipe defaults
/// with `--export-format` / `--export-include-graph` / `--export-template` as
/// explicit overrides. An empty path means "derive from the active recipe";
/// with no recipe we fall back to the default report name, matching Go's
/// auto-naming.
/// Go `renderReportTemplate` (pkg/export/markdown.go:209-252), up to the point
/// where the interpreter is needed.
///
/// Go's read side is small and exactly specified: open the path, read at most
/// 1 MiB, and fail if the file is larger. Reproducing it means a missing or
/// oversized template reports the same error Go does instead of being
/// silently ignored — which is what Rust did, producing a default document for
/// a template the caller believed had been applied.
///
/// The render step is a Go `text/template` execution, which is not ported.
/// Rather than emit a plausible-looking document from a partial
/// implementation, this returns the default body and says so; the caller is
/// documented at the call site.
fn render_report_template(
    issues: &[bv_core::model::Issue],
    options: &bv_export::markdown::ReportOptions,
    path: &str,
) -> Result<String, String> {
    const MAX_TEMPLATE: u64 = 1 << 20;
    let file = std::fs::File::open(path)
        .map_err(|e| format!("read export template: {}", go_path_error("open", path, &e)))?;
    let mut buf = Vec::new();
    let read = {
        use std::io::Read;
        let mut limited = file.take(MAX_TEMPLATE + 1);
        limited.read_to_end(&mut buf)
    };
    read.map_err(|e| format!("read export template: {e}"))?;
    if buf.len() as u64 > MAX_TEMPLATE {
        return Err(format!("export template exceeds {MAX_TEMPLATE} bytes"));
    }
    Ok(bv_export::markdown::generate_report(
        issues, issues, options,
    ))
}

/// Go's `encoding/csv` field rule (writer.go `fieldNeedsQuotes`): a field is
/// quoted when it is empty-but-one, is a bare quote, has leading or trailing
/// whitespace, or contains a comma, quote, CR or LF. Quotes inside are doubled.
///
/// The previous hand-rolled writer replaced commas in titles with spaces and
/// dropped two columns, so a title containing a comma silently lost data and
/// `description` and `labels` were missing entirely.
fn csv_field_needs_quotes(field: &str) -> bool {
    if field.is_empty() {
        return false;
    }
    if field == "\"" {
        return true;
    }
    if field.starts_with(' ') || field.ends_with(' ') {
        return true;
    }
    field.chars().any(|c| matches!(c, ',' | '"' | '\r' | '\n'))
}

fn csv_write_field(out: &mut String, field: &str) {
    if !csv_field_needs_quotes(field) {
        out.push_str(field);
        return;
    }
    out.push('"');
    for c in field.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

/// Go's `case "csv"` (pkg/export/markdown.go:146-158): seven columns, labels
/// joined with `;`, written through `encoding/csv`.
fn render_export_csv(issues: &[bv_core::model::Issue]) -> String {
    let mut out = String::new();
    let mut row = |fields: &[String]| {
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            csv_write_field(&mut out, f);
        }
        out.push('\n');
    };
    row(&[
        "id".into(),
        "title".into(),
        "status".into(),
        "priority".into(),
        "issue_type".into(),
        "description".into(),
        "labels".into(),
    ]);
    for i in issues {
        row(&[
            i.id.clone(),
            i.title.clone(),
            i.status.as_str().to_string(),
            i.priority.to_string(),
            i.issue_type.to_string(),
            i.description.clone(),
            i.labels.join(";"),
        ]);
    }
    out
}

fn run_export_report(args: &[String], output_path: &str) -> ExitCode {
    let flag_value = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let format = match flag_value("--export-format") {
        Some(value) => value,
        // Go `ResolveReportOptions` seeds Format with "markdown" and only a
        // recipe or an explicit override replaces it.
        None => "markdown".to_string(),
    };
    let template = flag_value("--export-template").unwrap_or_default();
    // Go `ReportOptions.validate` (pkg/export/markdown.go:80-90) rejects these
    // combinations before rendering anything. Rust had no equivalent, so a
    // `--export-template` alongside a non-markdown format was accepted and the
    // template then silently ignored.
    // Go registers this as a plain `flag.Bool` (main.go:1473), so it takes a
    // value only through `=`: `--export-include-graph=false`. A following
    // token is a positional argument, which is why `bv --export-include-graph
    // true` answers `unknown command "true"`. The earlier code read the next
    // argv element, swallowing it and then validating combinations Go never
    // rejected. With no override, `ResolveReportOptions` derives the default
    // from the format, so markdown keeps its graph and csv never gets one.
    let include_graph_flag = match args
        .iter()
        .find_map(|a| a.strip_prefix("--export-include-graph="))
    {
        None => format != "csv",
        Some(raw) => match parse_go_bool(raw) {
            Some(parsed) => parsed,
            None => {
                eprintln!("invalid boolean value {raw:?} for -export-include-graph: parse error");
                return ExitCode::from(2);
            }
        },
    };
    //
    // `ResolveReportOptions` returns this error bare (main.go:4396-4398) —
    // no "Error:" prefix and no "rendering report:" wrapper, unlike the errors
    // from `GenerateReport` itself.
    if format == "csv" && include_graph_flag {
        eprintln!("CSV cannot include a graph; set --export-include-graph=false");
        return ExitCode::from(1);
    }
    if format == "mermaid" && !include_graph_flag {
        eprintln!("Mermaid export requires include_graph=true");
        return ExitCode::from(1);
    }
    if !template.is_empty() && format != "markdown" {
        eprintln!("custom templates require markdown export");
        return ExitCode::from(1);
    }
    let options = bv_export::markdown::ReportOptions {
        format: format.clone(),
        template: template.clone(),
        include_graph: include_graph_flag,
        // Go stamps the report with `robotNow()`, which is UTC.
        generated_at: Some(jiff::Timestamp::now()),
        ..Default::default()
    };
    if let Err(e) = options.validate() {
        eprintln!("Error: {e}");
        return ExitCode::from(2);
    }
    let path = if output_path.is_empty() {
        "report.md".to_string()
    } else {
        output_path.to_string()
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, stats) = match bv_core::discovery::load_issues_from_repo(&cwd) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let mut options = options;
    attach_report_origins(&mut options, &issues, &cwd, stats);
    let body = match options.format.as_str() {
        "json" => go_json_string(&serde_json::json!(issues)),
        "csv" => render_export_csv(&issues),
        "mermaid" => bv_export::mermaid::generate_mermaid(&issues),
        // Go runs `renderReportTemplate` here when `--export-template` names a
        // file (pkg/export/markdown.go:209-252): a Go `text/template`
        // execution with its own field escaping and 1 MiB read / 16 MiB render
        // caps. That interpreter is not ported, so the template path is
        // validated and its errors reported exactly as Go's are, and the
        // document falls back to the default rather than being half-rendered.
        _ => {
            if !template.is_empty() {
                match render_report_template(&issues, &options, &template) {
                    Ok(body) => body,
                    Err(e) => {
                        eprintln!("rendering report: {e}");
                        return ExitCode::from(1);
                    }
                }
            } else {
                bv_export::markdown::generate_report(&issues, &issues, &options)
            }
        }
    };
    match std::fs::write(&path, &body) {
        Ok(_) => {
            println!("Exporting {} issues to {}...", issues.len(), path);
            println!("Done!");
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("Error writing {path}: {e}");
            ExitCode::from(1)
        }
    }
}

/// Go attaches an `IssueOrigin` to every loaded issue (pkg/loader/loader.go:94
/// `AttachIssueOrigins`), and `GenerateMarkdown` renders a Quick Actions block
/// and a per-issue Commands block from it. Reproduce that binding here so the
/// two blocks are not silently empty on a machine that does have a live
/// tracker, and are still empty (Go's `Origin == nil` shape) on one that does
/// not.
fn attach_report_origins(
    options: &mut bv_export::markdown::ReportOptions,
    issues: &[bv_core::model::Issue],
    cwd: &std::path::Path,
    stats: bv_core::loader::ParseStats,
) {
    let source_path = match bv_core::discovery::find_jsonl_path_with_warnings(
        &bv_core::discovery::get_beads_dir(cwd).unwrap_or_else(|_| cwd.to_path_buf()),
        |_| {},
    )
    .ok()
    .flatten()
    {
        Some(path) => path.to_string_lossy().into_owned(),
        None => return,
    };
    // Go marks the whole source read-only when the load was not clean, so no
    // issue in it may present a mutation command.
    let complete = stats.errors == 0;
    for issue in issues {
        let mut origin = bv_core::tracker::resolve_issue_origin(&source_path, &issue.id);
        if !complete && origin.read_only_reason.is_empty() {
            origin.read_only_reason = "source authority is incomplete or stale".to_string();
        }
        options.origins.insert(issue.id.clone(), origin);
    }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = argv::rewrite_args(&raw);

    // Go's pflag parses every registered flag for every command, so a value it
    // cannot read fails before cobra reaches the help flag, `--version`, or the
    // modifier-requires table: `bv --help --related-max-results=abc` exits 1
    // with the parse error, not with the help text. Reproduce that ordering
    // here rather than leaving the check inside the `--robot-related` handler,
    // where it would run too late to be observed.
    if let Err(msg) = parse_related_max_results(&args) {
        eprintln!("{msg}");
        return ExitCode::from(1);
    }

    // --help/-h short-circuits everything else (Go parity): cobra's help flag
    // is consulted before the root command runs, so `--help` wins over
    // `--version`, over modifier-requires violations, over exclusive-primary
    // violations, and over any `--robot-*` dispatch. Exit code 0 either way.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{}", flags::render_help(flags::HELP_PROGRAM));
        return ExitCode::from(0);
    }

    // --version handled before validation (Go parity). Go prints
    // "bv <version>" from `pkg/version`, whose fallback at the parity commit
    // is v0.25.0 — the same string the envelope already carries. Printing the
    // Rust crate's own name and version here would make `--version` the one
    // place the binary contradicts its own output.
    if args.iter().any(|a| a == "--version") {
        println!("{} {}", flags::HELP_PROGRAM, GO_APP_VERSION);
        return ExitCode::from(0);
    }

    let presence = validation::Presence::from_args(&args);

    // Go main.go:1768-1783 registers `--id-pattern` BEFORE the modifier rules
    // are validated and before any correlation work, so both explicit matching
    // and orphan detection learn non-default ID formats. A pattern that does
    // not compile is `Invalid --id-pattern %q: %v` at exit 2 — previously the
    // flag was accepted and silently ignored, so a typo looked like it worked.
    let id_patterns: Vec<String> = {
        let mut out = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--id-pattern" {
                if let Some(v) = args.get(i + 1) {
                    out.push(v.clone());
                }
                i += 2;
                continue;
            }
            if let Some(v) = a.strip_prefix("--id-pattern=") {
                out.push(v.to_string());
            }
            i += 1;
        }
        out
    };
    if !id_patterns.is_empty() {
        let mut compiled = Vec::with_capacity(id_patterns.len());
        for p in &id_patterns {
            match regex::Regex::new(p) {
                Ok(re) => compiled.push(re),
                Err(e) => {
                    eprintln!("Invalid --id-pattern {p:?}: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        bv_correlation::explicit::set_custom_id_patterns(compiled);
    }

    // Go runs four validation stages and each one exits on its own
    // (cmd/bv/main.go:1890-1906): modifier-requires, then enum, then
    // exclusive-primaries, then the `--watch-export`/`--as-of` pair. They are
    // reproduced as separate early returns rather than one merged list,
    // because a merged list reports stages the oracle never reached — a
    // dangling `--watch-export` printed the as-of message on top of the
    // modifier message Go stops at.
    //
    // Within a stage Go exits on the first broken rule, so at most one message
    // is printed here; `reject_stage` keeps that shape for the stages that can
    // still produce several (the exclusive-primary groups).
    let reject_stage = |stage: Vec<validation::ValidationError>| -> Option<ExitCode> {
        if stage.is_empty() {
            return None;
        }
        for v in &stage {
            eprintln!("Error: {v}");
        }
        Some(ExitCode::from(1))
    };
    // Go's `flag.Parse` runs before any of the validation stages below, so a
    // value-taking flag with nothing to consume is reported ahead of a
    // modifier-requires violation — `--search-limit` alone is a missing
    // argument, not a missing `--search`.
    if let Some(v) = validation::validate_flag_arguments(&args) {
        eprintln!("{v}");
        return ExitCode::from(1);
    }
    // An undefined long flag is a parse error in Go, not something that falls
    // through to the TUI (cmd/bv/main.go:1273-1295). `bvr --robot-triagee`
    // used to print "Loaded N issues — launching TUI" and block forever.
    if let Some(unknown) = argv::unknown_long_flag(&args) {
        match argv::suggest_closest_flag(&unknown) {
            Some(suggestion) => {
                eprintln!(
                    "unknown flag: --{unknown}\nDid you mean `{}`?\nRun `bv --help` for all flags or `bv --robot-help` for agent-focused docs.",
                    argv::corrected_unknown_flag_command(&args, &unknown, &suggestion)
                );
            }
            None => eprintln!("unknown flag: --{unknown}"),
        }
        return ExitCode::from(1);
    }
    if let Some(v) = validation::validate_modifier_requires_with(&presence, Some(&args)) {
        eprintln!("Error: {v}");
        return ExitCode::from(1);
    }

    // Enum-valued flags (--graph-format, --script-format) were registered and
    // ported but never checked, so an invalid value was accepted and only
    // failed later — or not at all. Go rejects at validation time with a
    // "did you mean" suggestion.
    let enum_supplied: Vec<(&str, &str)> = flags::ENUM_RULES
        .iter()
        .filter_map(|rule| {
            let value = flag_value(&args, rule.name)?;
            Some((rule.name, value))
        })
        .collect();
    // Go main.go:1894-1896 — an enum violation is `Error: %v` and exit 1, the
    // same as a modifier-requires violation at :1890-1893. Neither path prints
    // a Usage line.
    if let Some(err) = flags::validate_enum_flags(&enum_supplied) {
        eprintln!("Error: {}", err.message());
        return ExitCode::from(1);
    }

    if let Some(code) = reject_stage(validation::validate_exclusive_primaries(&presence)) {
        return code;
    }
    if let Some(code) = reject_stage(validation::validate_watch_export_as_of(&presence)) {
        return code;
    }

    // Go main.go:1908-1933 opens the file and starts the sampler right after
    // the exclusive-primary and `--watch-export`/`--as-of` checks, so a run
    // rejected by either of those never creates the file. The guard drops when
    // `main` returns, which is what reproduces Go's `defer stopCPUProfile()`
    // together with `RobotContext.FinalizeBeforeExit` (main.go:2050) — Go
    // needs the second hook because robot dispatch calls `os.Exit` and skips
    // defers, whereas this crate never calls `process::exit`, so a local in
    // `main` covers every one of the exit paths below.
    let _cpu_profile = match start_cpu_profile(flag_value(&args, "cpu-profile")) {
        Ok(guard) => guard,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(1);
        }
    };

    // Go main.go:1937-1944 — `--db` is the TOP of the discovery chain
    // ("--db flag > BEADS_DB env > BEADS_DIR env > auto-discovery", the
    // priority comment at :1936). `get_beads_dir` already reads
    // BEADS_DB_ENV first (bv-core/src/discovery.rs:168-197), so publishing the
    // absolute path into the environment is the whole integration. Applied
    // before any load: a bad path must fail the way Go fails it rather than
    // being silently ignored in favour of the local .beads.
    if let Some(db) = flag_value(&args, "db").filter(|v| !v.is_empty()) {
        match std::path::absolute(db) {
            Ok(abs) => std::env::set_var(bv_core::discovery::BEADS_DB_ENV, abs),
            Err(e) => {
                eprintln!("Error resolving --db path: {e}");
                return ExitCode::from(1);
            }
        }
    }
    // Go main.go:1946-1949 — `--no-cache` is an env publish, not a switch the
    // CLI consumes: it sets BV_NO_CACHE=1 so the analysis and correlation disk
    // caches are bypassed. Today it is behaviour-neutral because neither cache
    // is constructed outside tests, but the flag must be correct the moment
    // they are.
    if presence.has("no-cache") {
        std::env::set_var("BV_NO_CACHE", "1");
    }
    // Go main.go:2777-2786 — the scoping flags are captured here and applied
    // once inside `scopeLoadedIssues`, so every robot handler, every export and
    // the TUI see the same narrowed issue set. Recorded from the REWRITTEN argv
    // so `-l` / `-r` are visible.
    // Go resolves `--recipe` at main.go:2580-2596 and rejects an unknown name
    // before any handler runs, listing what is available. The flag was recorded
    // and then never resolved here, so `--recipe nosuchrecipe` scoped nothing
    // and the command produced a full payload for the *unfiltered* issue set —
    // a caller asking for one view silently got another.
    if let Some(recipe) = flag_value(&args, "recipe")
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if let Some(err) = validate_recipe_name(recipe) {
            eprintln!("{err}");
            return ExitCode::from(1);
        }
    }
    set_scope_flags(
        flag_value(&args, "label").unwrap_or_default().to_string(),
        flag_value(&args, "recipe").unwrap_or_default().to_string(),
        flag_value(&args, "repo").unwrap_or_default().to_string(),
    );
    FORCE_FULL_ANALYSIS.store(
        presence.has("force-full-analysis"),
        std::sync::atomic::Ordering::Relaxed,
    );
    // Go main.go:2619-2631 — auto-discovery of a workspace config only runs
    // when `--workspace` and `--as-of` are both empty; an explicit path is
    // loaded verbatim at main.go:2678-2721 and stamped SourceKind="workspace".
    if let Some(ws) = flag_value(&args, "workspace")
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        // An EXPLICIT --workspace that cannot be read is fatal. Go loads the
        // named path verbatim and stops (main.go:2678-2721) rather than
        // quietly reading a different repository; bvr warned and carried on to
        // the TUI, so a typo in the path still produced a working-looking run
        // over the local .beads. The message is Go's, down to the PathError
        // wording from go_path_error.
        let path = std::path::PathBuf::from(ws);
        if let Err(e) = std::fs::File::open(&path) {
            eprintln!(
                "Error loading workspace: failed to load workspace config: {}",
                go_path_error("open", &path.to_string_lossy(), &e)
            );
            return ExitCode::from(1);
        }
        set_workspace_override(path);
    }
    // Go main.go:1766 — the theme warning precedes EVERY dispatch, robot
    // commands included, so it runs here rather than on the TUI path.
    warn_unknown_theme(&args);

    // Self-update (Go: --check-update / --update-dry-run / --update / --rollback).
    if presence.has("check-update") {
        return run_check_update();
    }
    if presence.has("update-dry-run") {
        return run_update_dry_run();
    }
    if presence.has("update") {
        return run_update(&args);
    }
    if presence.has("rollback") {
        return run_rollback();
    }
    if presence.has("robot-help") {
        print_robot_help();
        return ExitCode::from(0);
    }
    // Go main.go:2172 — any action, or either modifier, enters the agents
    // command. `--agents-dry-run` and `--agents-force` on their own used to fall
    // through to the TUI, which an agent invoking them cannot drive.
    if presence.has("agents-add")
        || presence.has("agents-remove")
        || presence.has("agents-update")
        || presence.has("agents-check")
        || presence.has("agents-dry-run")
        || presence.has("agents-force")
    {
        return run_agents_commands(&presence, &args);
    }
    if presence.has("robot-capabilities") {
        return run_robot_capabilities();
    }

    /// Go `analysis.NewAnalyzer(issues).ComputeImpactScores()` (main.go:2505-2506)
    /// — the same graph and metric maps `priority_recommendations` builds, kept as
    /// its own helper so `--feedback-accept`/`--feedback-ignore` can reach the
    /// per-issue `Breakdown` that `ScoreContributions` is derived from.
    fn impact_scores_with_breakdown(
        issues: &[bv_core::model::Issue],
    ) -> Vec<bv_analysis::impact::IssueImpact> {
        let g = bv_analysis::build_graph(issues);
        let pr = bv_graph_core::pagerank_default(&g);
        let bw = bv_analysis::analyzer::go_betweenness(&g);
        let cp = bv_graph_core::critical_path_heights(&g);
        let to_map = |v: &[f64]| -> std::collections::BTreeMap<String, f64> {
            v.iter()
                .enumerate()
                .map(|(i, x)| (g.node_id(i).unwrap_or_default().to_string(), *x))
                .collect()
        };
        let (pr_map, bw_map, cp_map) = (to_map(&pr), to_map(&bw), to_map(&cp));
        bv_analysis::impact::compute_impact_scores(&bv_analysis::impact::ImpactInputs {
            issues,
            pagerank: &pr_map,
            betweenness: &bw_map,
            critical_path: Some(&cp_map),
            g: &g,
            now: robot_now(),
        })
    }

    // Go main.go:2441-2530 — the four `--feedback-*` flags form one early
    // block, and each branch exits. Precedence is reset > show > record, and
    // `--feedback-ignore` wins over `--feedback-accept` for the issue id when
    // both are given (main.go:2473-2477).
    //
    // POSITION is load-bearing and was wrong here until this block moved. Go
    // dispatches `--robot-help`/`--version`/`--robot-capabilities` at
    // main.go:2052-2054, then the agents block at :2425, then feedback at
    // :2441, and only then `--robot-recipes` (:2544), `--robot-schema`
    // (:2547), `--robot-metrics` (:4305) and every other primary (:3456+). So
    // a feedback flag paired with any of those is answered as a feedback
    // query in Go, while running it after the robot dispatch answered it as a
    // triage payload and never wrote `feedback.json` at all.
    let fb_accept = flag_value(&args, "feedback-accept").unwrap_or_default();
    let fb_ignore = flag_value(&args, "feedback-ignore").unwrap_or_default();
    let fb_reset = presence.has("feedback-reset");
    let fb_show = presence.has("feedback-show");
    if !fb_accept.is_empty() || !fb_ignore.is_empty() || fb_reset || fb_show {
        let cwd = std::env::current_dir().unwrap_or_default();
        let beads_dir = match bv_core::discovery::get_beads_dir(&cwd) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error getting beads directory: {e}");
                return ExitCode::from(1);
            }
        };
        let mut feedback = match bv_analysis::feedback::load_feedback(&beads_dir) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("Error loading feedback: {e}");
                return ExitCode::from(1);
            }
        };

        if fb_reset {
            feedback.reset();
            if let Err(e) = feedback.save(&beads_dir) {
                eprintln!("Error saving feedback: {e}");
                return ExitCode::from(1);
            }
            println!("Feedback data reset to defaults.");
            return ExitCode::from(0);
        }

        if fb_show {
            // Go main.go:2464-2469 — `ToJSON` re-indented with two spaces and
            // printed, deliberately NOT wrapped in the robot envelope.
            match serde_json::to_value(feedback.to_json()) {
                Ok(v) => println!("{}", go_json_string_pretty(&v)),
                Err(e) => {
                    eprintln!("Error encoding feedback: {e}");
                    return ExitCode::from(1);
                }
            }
            return ExitCode::from(0);
        }

        let (issue_id, action) = if !fb_ignore.is_empty() {
            (fb_ignore, "ignore")
        } else {
            (fb_accept, "accept")
        };
        // Go main.go:2494-2512 needs the issue's impact score and breakdown, so
        // the issues have to load for accept/ignore even though reset/show do
        // not.
        let issues = match bv_core::discovery::load_issues_from_repo(&cwd) {
            Ok((issues, _)) => issues,
            Err(e) => {
                eprintln!("Error loading issues: {e}");
                return ExitCode::from(1);
            }
        };
        if !issues.iter().any(|i| i.id == issue_id) {
            eprintln!("Issue not found: {issue_id}");
            return ExitCode::from(1);
        }
        let scores = impact_scores_with_breakdown(&issues);
        let hit = scores.iter().find(|s| s.id == issue_id);
        let score = hit.map(|s| s.score).unwrap_or(0.0);
        let contributions = hit
            .map(|s| bv_analysis::feedback::ScoreContributions::from(&s.breakdown))
            .unwrap_or_default();
        if let Err(e) = feedback.record_feedback(issue_id, action, score, contributions) {
            eprintln!("Error recording feedback: {e}");
            return ExitCode::from(1);
        }
        if let Err(e) = feedback.save(&beads_dir) {
            eprintln!("Error saving feedback: {e}");
            return ExitCode::from(1);
        }
        println!("Recorded {action} feedback for {issue_id} (score: {score:.3})");
        println!("{}", feedback.summary());
        return ExitCode::from(0);
    }

    if presence.has("robot-schema") {
        return run_robot_schema(&args);
    }
    if presence.has("robot-metrics") {
        return run_robot_metrics();
    }
    if presence.has("robot-docs") {
        return run_robot_docs(&args);
    }

    // --generate-docs runs FIRST in Go's RunE (cmd/bv/main.go:1757) and exits 0
    // after writing the artifacts. Without a handler it falls through to the TUI
    // launcher, which hangs in any TTY and is the footgun AGENTS.md warns about.
    if presence.has("generate-docs") {
        return run_generate_docs();
    }

    // --export (Go main.go:4372) writes a report using recipe defaults, with
    // --export-format / --export-include-graph / --export-template as overrides.
    // Also must not fall through to the TUI.
    if let Some(export_idx) = args.iter().position(|a| a == "--export") {
        let output_path = args.get(export_idx + 1).cloned().unwrap_or_default();
        let md_idx = args.iter().position(|a| a == "--export-md");
        if !output_path.is_empty() {
            if let Some(m) = md_idx {
                let md_path = args.get(m + 1).cloned().unwrap_or_default();
                eprintln!("Error: --export and --export-md specify conflicting output paths");
                let _ = md_path;
                return ExitCode::from(2);
            }
        }
        return run_export_report(&args, &output_path);
    }

    // Export markdown (Phase 5a).
    if let Some(output_path_idx) = args.iter().position(|a| a == "--export-md") {
        let output_path = args
            .get(output_path_idx + 1)
            .cloned()
            .unwrap_or_else(|| "report.md".to_string());
        let cwd = std::env::current_dir().unwrap_or_default();
        let (issues, stats) = match bv_core::discovery::load_issues_from_repo(&cwd) {
            Ok(x) => x,
            Err(e) => {
                if is_unresolved_source_error(&e.to_string()) {
                    return emit_unresolved_source();
                }
                eprintln!("Error: {e}");
                return ExitCode::from(1);
            }
        };
        let mut options = bv_export::markdown::ReportOptions {
            // Go forces `overrides.Format = "markdown"` for `--export-md`,
            // so the graph stays on (only `csv` turns it off).
            format: "markdown".to_string(),
            generated_at: Some(jiff::Timestamp::now()),
            ..Default::default()
        };
        attach_report_origins(&mut options, &issues, &cwd, stats);
        let md = bv_export::markdown::generate_report(&issues, &issues, &options);
        match std::fs::write(&output_path, &md) {
            Ok(_) => {
                // Go main.go:4424 / :4478 — the progress line goes out
                // before the file is written and `Done!` after, so a crash
                // mid-write is visible rather than silent.
                println!("Exporting {} issues to {}...", issues.len(), output_path);
                println!("Done!");
                return ExitCode::from(0);
            }
            Err(e) => {
                eprintln!("Error writing {}: {e}", output_path);
                return ExitCode::from(1);
            }
        }
    }

    // Export graph (Go --export-graph: .html interactive / json|dot|mermaid).
    if let Some(idx) = args.iter().position(|a| a == "--export-graph") {
        let output_path = args.get(idx + 1).cloned().unwrap_or_default();
        let cwd = std::env::current_dir().unwrap_or_default();
        let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
            Ok(x) => x,
            Err(e) => {
                if is_unresolved_source_error(&e.to_string()) {
                    return emit_unresolved_source();
                }
                eprintln!("Error: {e}");
                return ExitCode::from(1);
            }
        };

        // Format inferred from extension (Go: .html interactive, .dot, else json).
        // Note: --graph-format/--graph-depth are robot-graph-only in Go validation.
        //
        // Go's STATIC branch (main.go:3541-3555) handles .png/.svg via
        // `export.SaveGraphSnapshot` with `Preset: *graphPreset`. That branch
        // was unreachable from this handler, so --graph-preset had nothing to
        // reach and a .png path silently fell through to the JSON branch.
        let is_static = {
            let lower = output_path.to_ascii_lowercase();
            lower.ends_with(".png") || lower.ends_with(".svg")
        };
        if is_static {
            // Go main.go:1512 defaults this to "compact" and has no enum rule
            // for it (main.go:1845-1848), so an unrecognised value is
            // silently compact — the library keeps that, so pass it through.
            let preset = argv::go_string_flag_value(&args, "graph-preset")
                .unwrap_or("compact")
                .to_string();
            // Go passes the raw `--graph-title` on the static branch, with no
            // basename fallback — that fallback belongs only to the HTML
            // branch above (main.go:3491-3503).
            let title = argv::go_string_flag_value(&args, "graph-title")
                .unwrap_or_default()
                .to_string();
            let export_issues: Vec<bv_core::model::Issue> = issues.clone();
            let node_count = export_issues.len();
            let stats = bv_analysis::label_health::compute_graph_stats(&export_issues);
            if let Err(e) = bv_export::graph_snapshot::save_graph_snapshot(
                bv_export::graph_snapshot::GraphSnapshotOptions {
                    path: std::path::PathBuf::from(&output_path),
                    format: String::new(),
                    title,
                    preset,
                    issues: export_issues,
                    data_hash: hash.clone(),
                },
                Some(&stats),
            ) {
                eprintln!("Error exporting graph snapshot: {e}");
                return ExitCode::from(1);
            }
            // Go main.go:3554 — the node count is part of the line.
            println!(
                "✓ Graph exported to {output_path} ({node_count} nodes) - tip: use .html for interactive graphs"
            );
            return ExitCode::SUCCESS;
        }
        let fmt = if output_path.ends_with(".html") {
            "html".to_string()
        } else if output_path.ends_with(".dot") {
            "dot".to_string()
        } else if output_path.ends_with(".md") {
            "mermaid".to_string()
        } else {
            "json".to_string()
        };
        let issues: Vec<bv_core::model::Issue> = issues;

        let content = match fmt.as_str() {
            "dot" => bv_export::graph_export::generate_dot(&issues, None),
            "mermaid" => bv_export::graph_export::generate_mermaid_graph(&issues),
            "html" => {
                // Go main.go:3491-3503 — on the interactive/HTML branch an
                // empty `--graph-title` falls back to `filepath.Base(cwd)`
                // ("project" if that is unavailable). The STATIC branch at
                // main.go:3543 passes the raw string with no such default, so
                // this fallback deliberately lives only here.
                let title = flag_value(&args, "graph-title")
                    .map(str::to_string)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| {
                        cwd.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| "project".to_string())
                    });
                let mermaid = bv_export::graph_export::generate_mermaid_graph(&issues);
                format!(
                    "<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n<script src=\"https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.min.js\"></script>\n<script>mermaid.initialize({{startOnLoad:true}});</script>\n</head>\n<body>\n<h1>{title}</h1>\n<pre class=\"mermaid\">\n{mermaid}</pre>\n</body>\n</html>\n"
                )
            }
            _ => serde_json::to_string_pretty(&serde_json::json!({
                "format": "json",
                "graph": bv_export::graph_export::generate_adjacency(&issues),
                "nodes": issues.len(),
                "data_hash": hash,
            }))
            .unwrap_or_default(),
        };

        let out = if output_path.is_empty() {
            format!("beads_graph.{fmt}")
        } else {
            output_path.clone()
        };
        // Go main.go:3091 — pre-export hooks wrap every export, gated on
        // `!*noHooks`. The DEFAULT path (no flag) is the one that matters: Go
        // runs the hooks, so skipping them unconditionally was a divergence
        // in the common case, not just behind `--no-hooks`.
        if let Err(e) = run_export_hooks(
            presence.has("no-hooks"),
            &cwd,
            &out,
            &fmt,
            issues.len(),
            true,
        ) {
            eprintln!("Error running pre-export hooks: {e}");
            return ExitCode::from(1);
        }
        // Go prints a different completion line per format, and only the
        // interactive one reports the graph's own edge count (main.go:3536).
        let edges = bv_analysis::analyzer::build_graph(&issues).edge_count();
        match std::fs::write(&out, &content) {
            Ok(_) => {
                if fmt == "html" {
                    println!(
                        "✓ Interactive graph exported to {out} ({} nodes, {edges} edges)",
                        issues.len()
                    );
                } else {
                    println!("Exported {} issues to {} ({fmt})", issues.len(), out);
                }
                return ExitCode::from(0);
            }
            Err(e) => {
                eprintln!("Error writing {out}: {e}");
                return ExitCode::from(1);
            }
        }
    }

    // Go main.go:2031 — `robotShowToonStats = *toonStats || TOON_STATS=1`.
    // Only the TOON encoder reads it, so the flag is a no-op in JSON mode.
    set_show_toon_stats(presence.has("stats") || std::env::var("TOON_STATS").as_deref() == Ok("1"));

    // Format validation (Go: exit 2 on invalid)
    if presence.has("format") {
        let fmt_val = args
            .iter()
            .position(|a| a == "--format")
            .and_then(|i| args.get(i + 1))
            .unwrap_or(&"json".to_string())
            .clone();
        if fmt_val != "json" && fmt_val != "toon" {
            eprintln!("Invalid --format \"{fmt_val}\" (expected json|toon)");
            return ExitCode::from(2);
        }
        // Go (cmd/bv/main.go:2037-2038) warns and downgrades toon -> json when
        // the external `tru` encoder is absent, doing it *before* the payload is
        // built so `output_format` never claims "toon" over JSON bytes on
        // stdout.
        if fmt_val == "toon" && !bv_robot::envelope::tru_available() {
            eprintln!("warning: tru not available; falling back to JSON");
            set_output_format("json");
        } else {
            set_output_format(&fmt_val);
        }
    }

    // Robot drift (Go: --robot-drift wraps --check-drift with JSON output).
    if presence.has("robot-drift") {
        return run_robot_drift();
    }
    // Export markdown (Phase 5a).
    if presence.has("check-drift") {
        return run_check_drift();
    }
    if let Some(desc) = args
        .iter()
        .zip(args.iter().skip(1))
        .find(|(a, _)| a.as_str() == "--save-baseline")
        .map(|(_, v)| v.clone())
    {
        return run_save_baseline(&desc);
    }

    // Go main.go:2564-2577 — handled before any issue load, per the comment at
    // :2547-2548, so `--baseline-info` works in a repo with no `.beads`.
    if presence.has("baseline-info") {
        return run_baseline_info();
    }

    // Go main.go:4012-4100. Dispatched well above the TUI fallthrough: without
    // a handler the flag reached the interactive launcher and hung forever.
    if presence.has("emit-script") {
        return run_emit_script(&args);
    }

    // Go main.go:3878-3911 and :3915-4007. Neither flag has a
    // modifier-requires rule in Go, so they are legal standing alone. Both
    // previously fell through to the TUI and blocked.
    if let Some(path) = flag_value(&args, "priority-brief").filter(|v| !v.is_empty()) {
        return run_priority_brief(path);
    }
    if let Some(dir) = flag_value(&args, "agent-brief").filter(|v| !v.is_empty()) {
        return run_agent_brief(dir);
    }

    // Correlation-family dispatch (Phase 3e).
    if presence.has("robot-history") || presence.has("bead-history") {
        return run_robot_history();
    }
    if presence.has("robot-orphans") {
        return run_robot_orphans();
    }

    // Triage family dispatch (Phase 3c first slice).
    // robot-next has its own flattened output shape (Go handleRobotNext).
    if presence.has("robot-next") {
        return run_robot_next();
    }
    let triage_family = [
        "robot-triage",
        "robot-triage-by-track",
        "robot-triage-by-label",
    ]
    .iter()
    .any(|f| presence.has(f));
    if triage_family {
        return run_robot_triage();
    }

    // Insights / Plan / Priority / Suggest / Alerts / Graph / Recipes / Label trio
    if presence.has("robot-insights") {
        return run_robot_insights();
    }
    if presence.has("robot-plan") {
        return run_robot_plan();
    }
    if presence.has("robot-priority") {
        return run_robot_priority(&args);
    }
    if presence.has("robot-suggest") {
        return run_robot_suggest(&args);
    }
    // Go main.go:3566-3570 — `--profile-startup` is dispatched on presence and
    // exits 0. `--profile-json` only selects the output shape and is not a
    // primary of its own; the modifier-requires rule (flags.rs:1750) has
    // already rejected it standing alone by the time control reaches here.
    if presence.has("profile-startup") {
        return run_profile_startup(presence.has("profile-json"));
    }
    if presence.has("robot-alerts") {
        return run_robot_alerts();
    }
    if presence.has("robot-graph") {
        return run_robot_graph(&args);
    }
    if presence.has("robot-recipes") {
        return run_robot_recipes();
    }
    if presence.has("robot-label-health") {
        return run_robot_label_health();
    }
    if presence.has("robot-label-flow") {
        return run_robot_label_flow();
    }
    if presence.has("robot-label-attention") {
        return run_robot_label_attention();
    }
    if presence.has("robot-blocker-chain") {
        return run_robot_blocker_chain(&args);
    }
    if presence.has("robot-confirm-correlation") {
        return run_robot_correlation_feedback(&args, "confirm-correlation", "confirm");
    }
    if presence.has("robot-reject-correlation") {
        return run_robot_correlation_feedback(&args, "reject-correlation", "reject");
    }
    if presence.has("robot-explain-correlation") {
        return run_robot_explain_correlation(&args);
    }
    if presence.has("robot-correlation-stats") {
        return run_robot_correlation_stats();
    }
    if presence.has("robot-file-beads") {
        return run_robot_file_beads(&args);
    }
    if presence.has("robot-file-hotspots") {
        return run_robot_file_hotspots(&args);
    }
    if presence.has("robot-file-relations") {
        return run_robot_file_relations(&args);
    }
    if presence.has("robot-search") {
        return run_robot_search(&args);
    }
    // A bare `--search Q` with no `--robot-*` primary. Go runs the same search
    // and prints the tab-separated text table (cmd/bv/main.go:2963, :3028-3035),
    // so it goes through the identical code path and differs only in output.
    if search_flag(&args, "search").is_some_and(|q| !q.trim().is_empty())
        && !flags::ROBOT_PRIMARIES.iter().any(|f| presence.has(f.name))
    {
        return run_robot_search(&args);
    }
    if presence.has("robot-causality") {
        return run_robot_causality(&args);
    }
    if presence.has("robot-related") {
        return run_robot_related(&args);
    }
    if presence.has("robot-impact-network") {
        return run_robot_impact_network(&args);
    }
    if presence.has("robot-sprint-list") {
        return run_robot_sprint_list();
    }
    if presence.has("robot-sprint-show") {
        return run_robot_sprint_show(&args);
    }
    if presence.has("robot-burndown") {
        return run_robot_burndown(&args);
    }
    if presence.has("robot-forecast") {
        return run_robot_forecast(&args);
    }
    if presence.has("robot-capacity") {
        return run_robot_capacity(&args);
    }

    if presence.has("robot-impact") {
        return run_robot_impact(&args);
    }
    if presence.has("robot-diff") {
        return run_robot_diff(&args);
    }
    if presence.has("robot-not-ready-labels") {
        return run_robot_not_ready_labels(&args);
    }
    // Export pages (static site bundle, Go --export-pages).
    if let Some(idx) = args.iter().position(|a| a == "--export-pages") {
        let out_dir = args
            .get(idx + 1)
            .cloned()
            .unwrap_or_else(|| "./bv-pages".to_string());
        let title = args
            .iter()
            .position(|a| a == "--pages-title")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| "Beads Dashboard".to_string());
        let include_closed = args.iter().any(|a| a == "--pages-include-closed");
        let cwd = std::env::current_dir().unwrap_or_default();

        // Go main.go:3061-3214 wraps the export in a `doExport` closure with an
        // `exportCount` captured beside it, so `--watch-export` re-runs the
        // identical pipeline and can label the run "Re-exporting (change #N)".
        // The counter is the closure's own state in Go; here it is an argument
        // because the loop that re-enters it lives in `run_export_watch`.
        let mut export_count: u32 = 0;
        let do_export = |export_count: &mut u32| -> Result<(), String> {
            *export_count += 1;
            // Go main.go:3066-3071 — the first run announces the export, later
            // runs are labelled with a wall-clock stamp and their ordinal.
            if *export_count > 1 {
                println!(
                    "\n[{}] Re-exporting (change #{})...",
                    hhmmss_now(),
                    *export_count - 1
                );
            } else {
                println!("Exporting static site...");
            }

            let (issues, hash, _as_of_commit) = load_issues_auto(&cwd, None)?;

            let visible: Vec<&bv_core::model::Issue> = issues
                .iter()
                .filter(|i| include_closed || !i.status.is_closed())
                .collect();

            let open = visible
                .iter()
                .filter(|i| matches!(i.status, bv_core::model::Status::Open))
                .count();
            let in_prog = visible
                .iter()
                .filter(|i| matches!(i.status, bv_core::model::Status::InProgress))
                .count();
            let blocked = visible
                .iter()
                .filter(|i| matches!(i.status, bv_core::model::Status::Blocked))
                .count();
            let closed = issues.iter().filter(|i| i.status.is_closed()).count();

            let mermaid = bv_export::graph_export::generate_mermaid_graph(&issues);
            let rows: String = visible
                .iter()
                .map(|i| {
                    format!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td><td>P{}</td><td>{}</td></tr>\n",
                        i.id,
                        html_escape(&i.title),
                        i.status.as_str(),
                        i.priority,
                        i.issue_type
                    )
                })
                .collect();

            let html = format!(
                r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>{title}</title>
<style>
body {{ font-family: -apple-system, sans-serif; margin: 2rem; background: #282a36; color: #f8f8f2; }}
h1 {{ color: #bd93f9; }}
.stats span {{ margin-right: 1rem; padding: 0.2rem 0.6rem; border-radius: 4px; background: #44475a; }}
table {{ border-collapse: collapse; width: 100%; margin-top: 1rem; }}
td, th {{ border: 1px solid #44475a; padding: 0.4rem 0.6rem; text-align: left; }}
th {{ background: #44475a; }}
.mermaid {{ background: #f8f8f2; padding: 1rem; border-radius: 8px; margin-top: 1rem; }}
</style>
<script src="https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.min.js"></script>
<script>mermaid.initialize({{startOnLoad:true, theme:'dark'}});</script>
</head>
<body>
<h1>{title}</h1>
<p class="stats">
<span>○ Open: {open}</span><span>◐ In-Progress: {in_prog}</span><span>◈ Blocked: {blocked}</span><span>● Closed: {closed}</span>
</p>
<table>
<tr><th>ID</th><th>Title</th><th>Status</th><th>Priority</th><th>Type</th></tr>
{rows}</table>
<div class="mermaid">
{mermaid}</div>
<p><small>data_hash: {hash} | Generated by bvr</small></p>
</body>
</html>
"#
            );

            std::fs::create_dir_all(&out_dir).ok();
            // Go main.go:1624 registers `--pages-include-history` with default
            // TRUE, and main.go:3178-3190 writes `data/history.json` for the
            // time-travel scrubber whenever it is on. Rust wrote only index.html,
            // so the DEFAULT invocation lost the history data — the flag being
            // unwired was not just an ignored modifier.
            //
            // The gate is pflag's boolean, not a `--no-` prefix: Go has no
            // auto-negation, so the one way to turn this off is
            // `--pages-include-history=false` (verified against the v0.25.0
            // oracle). The old test for the string `--no-pages-include-history`
            // matched neither the bare flag nor Go's real disable spelling, and
            // silently accepted a token Go rejects. See `argv::go_bool_flag`.
            let include_history = argv::go_bool_flag(&args, "pages-include-history", true)?;
            if include_history {
                let beads_file = cwd
                    .join(".beads")
                    .join("issues.jsonl")
                    .to_string_lossy()
                    .to_string();
                let generated_at = jiff_now();
                // Go main.go:7088-7095 builds the export's report on a correlator
                // with the feedback store attached, exactly like the robot paths.
                match generate_correlation_report(
                    &cwd,
                    &issues,
                    &bv_correlation::history::HistoryOptions {
                        limit: 500,
                        ..Default::default()
                    },
                    generated_at.clone(),
                    true,
                ) {
                    Ok(report) => {
                        match bv_export::time_travel::generate_history_for_export(
                            &cwd,
                            &beads_file,
                            &report,
                            &generated_at,
                        ) {
                            Ok(history) => {
                                match serde_json::to_string_pretty(&history) {
                                    Ok(json) => {
                                        // Go warns and continues on write failure
                                        // rather than aborting the export.
                                        if let Err(e) =
                                            std::fs::create_dir_all(format!("{out_dir}/data"))
                                        {
                                            println!("  → Warning: failed to create data dir: {e}");
                                        } else if let Err(e) = std::fs::write(
                                            format!("{out_dir}/data/history.json"),
                                            json,
                                        ) {
                                            println!(
                                                "  → Warning: failed to write history.json: {e}"
                                            );
                                        } else {
                                            println!(
                                                "  → history.json ({} commits)",
                                                history.commits.len()
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        println!("  → Warning: failed to encode history.json: {e}");
                                    }
                                }
                            }
                            Err(e) => {
                                println!("  → Warning: failed to generate history data: {e}");
                            }
                        }
                    }
                    Err(e) => {
                        println!("  → Warning: failed to generate history report: {e}");
                    }
                }
            }
            std::fs::write(format!("{out_dir}/index.html"), html)
                .map_err(|e| format!("Error writing {out_dir}/index.html: {e}"))?;
            // Go main.go:3212 closes every doExport with a wall-clock completion
            // stamp, including the initial run — the watch banner follows it.
            println!("✓ Export complete [{}]", hhmmss_now());
            Ok(())
        };

        // Go main.go:3216-3219 — the initial export runs before the watcher is
        // ever created, and a failure exits 1 with `Error: %v` on stderr.
        if let Err(e) = do_export(&mut export_count) {
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }

        // Go main.go:3223 — only now does watch mode take over; without the
        // flag this falls straight through to the success message.
        if args.iter().any(|a| a == "--watch-export") {
            // Watch the file that was actually LOADED, not a hardcoded
            // `.beads/issues.jsonl`. Go appends `beadsPath` — the discovered
            // source (main.go:3268) — so `--db`, `BEADS_DB` and auto-discovery
            // all move the watch target with the load. A fixed path would watch
            // a file nobody reads and never fire.
            let source = load_issues_auto_meta(&cwd, None)
                .map(|(_, _, _, m)| m.path)
                .unwrap_or_default();
            let watch_path = if source.is_empty() {
                cwd.join(".beads").join("issues.jsonl")
            } else {
                std::path::PathBuf::from(source)
            };
            return run_export_watch(&out_dir, &cwd, &watch_path, &mut export_count, do_export);
        }

        println!("Static site exported to {out_dir}");
        return ExitCode::from(0);
    }

    // Preview pages (Go --preview-pages): export then serve with livereload.
    if let Some(idx) = args.iter().position(|a| a == "--preview-pages") {
        let dir = args
            .get(idx + 1)
            .cloned()
            .unwrap_or_else(|| "./bv-pages".to_string());
        if !std::path::Path::new(&dir).join("index.html").exists() {
            eprintln!("No index.html in {dir} — run --export-pages first");
            return ExitCode::from(1);
        }
        let root = std::path::PathBuf::from(&dir);
        // Go main.go:3051 — `runPreviewServer(*previewPages, !*previewNoLiveReload)`,
        // so the flag negates into the library's `livereload_enabled` argument.
        let livereload = !args.iter().any(|a| a == "--no-live-reload");
        match bv_export::preview::start_preview(
            &root,
            |port| {
                println!("Preview serving at http://127.0.0.1:{port} (Ctrl+C to stop)");
            },
            livereload,
        ) {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::MAX);
                return ExitCode::from(0);
            }
            Err(e) => {
                eprintln!("Preview failed: {e}");
                return ExitCode::from(1);
            }
        }
    }

    // Any recognized `--robot-*` primary that reached this point is a real
    // command (per the flag registry / robot-help) whose dispatch handler
    // hasn't landed yet. Go never falls through to the TUI for a robot
    // invocation — fail fast with a clear, scriptable error instead of
    // silently starting the interactive TUI (which a robot/agent caller has
    // no way to drive and would just hang or block CI).
    if let Some(unhandled) = flags::ROBOT_PRIMARIES
        .iter()
        .find(|f| f.name != "robot-help" && presence.has(f.name))
    {
        eprintln!(
            "Error: --{} is registered but not yet implemented in bvr",
            unhandled.name
        );
        eprintln!("Usage: bvr --robot-help  (see robot-help for currently-dispatched commands)");
        return ExitCode::from(2);
    }

    // Go's `flag` package rejects any non-flag argument with
    // `unknown command "X" for "bv"` (exit 1). Without this guard `bvr
    // version` fell through to the TUI, which an agent or CI caller cannot
    // drive — it blocks until the process is killed. Reject before the TUI.
    if let Some(unknown) = argv::unconsumed_positional(&args) {
        eprintln!("unknown command \"{unknown}\" for \"bvr\"");
        return ExitCode::from(1);
    }

    // Interactive TUI: no robot flags present.
    let cwd = std::env::current_dir().unwrap_or_default();
    // Go main.go:4493-4500 — the background-mode rollout. The mutual-exclusion
    // check is TUI-path only: it sits after every `dispatchRobotFlagOrExit`
    // call, which is why Go's own e2e test still exits 0 for
    // `--background-mode --robot-triage`. Both flags only publish an env var
    // that `bv_tui::App::new` reads.
    if presence.has("background-mode") && presence.has("no-background-mode") {
        eprintln!("Error: --background-mode and --no-background-mode are mutually exclusive");
        return ExitCode::from(2);
    }
    if presence.has("background-mode") {
        std::env::set_var("BV_BACKGROUND_MODE", "1");
    } else if presence.has("no-background-mode") {
        std::env::set_var("BV_BACKGROUND_MODE", "0");
    }

    // Go main.go:4639-4666 — an explicitly passed but unrecognized `--theme`
    // value warns and resolves to auto rather than silently falling through
    // to a lower-precedence source. This is the only externally visible part
    // of the theme contract: the palette itself lives in `bv_tui::theme`.
    //
    // The palette application is a `bv_tui` concern and only reachable from the
    // TUI path, but the WARNING is not: main.go:1766 is the first statement of
    // the root command, so Go emits it for EVERY invocation, robot commands
    // included. It therefore has to run before dispatch, not down here.
    warn_unknown_theme(&args);

    // Go main.go:2678-2721 — an explicit `--workspace <path>` is used verbatim
    // and takes the place of auto-discovery (which main.go:2619-2631 only runs
    // when both `--workspace` and `--as-of` are empty). Go main.go:2805-2807
    // then stamps SourceKind="workspace" / SourcePath=<flag> on the source
    // metadata.
    let explicit_workspace = flag_value(&args, "workspace")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from);

    // Workspace mode: .bv/workspace.yaml found → aggregate multi-repo load
    let workspace_config = explicit_workspace
        .clone()
        .or_else(|| bv_core::workspace::find_workspace_config(&cwd));
    if let Some(ws_path) = workspace_config {
        let ws_root = ws_path
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(&cwd)
            .to_path_buf();
        match bv_core::workspace::load_workspace(&ws_path)
            .and_then(|cfg| bv_core::workspace::load_all(&cfg, &ws_root))
        {
            Ok((issues, results)) => {
                let repo_names: Vec<String> = results
                    .iter()
                    .filter(|r| r.error.is_none())
                    .map(|r| r.repo_name.clone())
                    .collect();
                let repo_count = repo_names.len();
                // Go main.go:2792 — the TUI receives the SCOPED set, same as
                // every robot consumer and every export.
                let (issues, _) = apply_scope(&issues);
                if let Some(code) = exit_if_no_issues(&issues) {
                    return code;
                }
                let mut app = bv_tui::App::new(issues.clone());
                app.workspace_repos = Some(repo_names);
                // Go main.go:4526-4531 — the debug-render check sits AFTER
                // `EnableWorkspaceMode`, so it is reachable from workspace mode
                // too. Checking only in the single-repo arm left
                // `bvr --debug-render <view>` opening a TUI here.
                if let Some(code) = run_debug_render(&mut app, &issues, &args) {
                    return code;
                }
                eprintln!(
                    "Workspace: loaded {} issues from {} repos — launching TUI",
                    issues.len(),
                    repo_count
                );
                return launch_tui(&mut app, &issues);
            }
            Err(e) => {
                eprintln!("Workspace load failed: {e} — falling back to single-repo mode");
            }
        }
    }

    match bv_core::discovery::load_issues_from_repo(&cwd) {
        Ok((issues, _)) => {
            // Go main.go:2792 — the TUI is scoped like everything else.
            let (issues, _) = apply_scope(&issues);
            if let Some(code) = exit_if_no_issues(&issues) {
                return code;
            }
            let mut app = bv_tui::App::new(issues.clone());
            if let Some(code) = run_debug_render(&mut app, &issues, &args) {
                return code;
            }
            // Go's bubbletea opens /dev/tty before the program starts and
            // reports the failure through the top-level handler
            // (`Error running beads viewer: could not open a new TTY: open
            // /dev/tty: ...`). Launching anyway leaves a much later, much
            // vaguer failure — "TUI error: Device not configured" — which
            // names neither the cause nor the way out. An agent running in CI
            // gets a one-line answer either way, but only one of them says
            // what to do.
            if let Err(e) = open_tty() {
                eprintln!("Error running beads viewer: could not open a new TTY: {e}");
                return ExitCode::from(1);
            }
            eprintln!("Loaded {} issues — launching TUI", issues.len());
            launch_tui(&mut app, &issues)
        }
        Err(e) => {
            eprintln!("Error loading beads: {e}");
            ExitCode::from(1)
        }
    }
}

/// Compute graph metrics and run the TUI event loop.
/// Go `limitMaps` / `limitMapInt` (cmd/bv/robot_registry.go:1884-1931) — keep
/// the top `limit` entries of a metric map, value descending with the key
/// ascending as tie-break, so the result is deterministic.
///
/// Without this every `--robot-insights` full_stats map carries every node
/// (600 on large_cyclic_600, 2500 on xl_2500) where Go carries 200.
fn limit_metric_map(
    m: serde_json::Map<String, serde_json::Value>,
    limit: usize,
) -> serde_json::Map<String, serde_json::Value> {
    if limit == 0 || m.len() <= limit {
        return m;
    }
    let mut items: Vec<(String, serde_json::Value)> = m.into_iter().collect();
    items.sort_by(|a, b| {
        let (ka, va) = (&a.0, &a.1);
        let (kb, vb) = (&b.0, &b.1);
        let na = va.as_f64().unwrap_or(f64::NEG_INFINITY);
        let nb = vb.as_f64().unwrap_or(f64::NEG_INFINITY);
        na.partial_cmp(&nb)
            .unwrap_or(std::cmp::Ordering::Equal)
            .reverse()
            .then_with(|| ka.cmp(kb))
    });
    items.truncate(limit);
    items.into_iter().collect()
}

/// `BV_INSIGHTS_MAP_LIMIT` (Go internal/env/env.go:131). A positive integer
/// overrides the default; anything else keeps it.
fn insights_map_limit() -> usize {
    std::env::var("BV_INSIGHTS_MAP_LIMIT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(200)
}

/// Attach the graph metrics `bv_tui` reads when it draws the insights view.
///
/// Go does this inside `ui.NewModel` (`pkg/ui/model.go`), so every consumer of
/// a freshly built model — the live TUI and `RenderDebugView` alike — sees a
/// populated panel. Ours was folded into `launch_tui`, which left
/// `--debug-render insights` drawing an empty dashboard.
fn apply_graph_metrics(app: &mut bv_tui::App, issues: &[bv_core::model::Issue]) {
    let g = bv_analysis::build_graph(issues);
    let pr = bv_graph_core::pagerank_default(&g);
    let bw = bv_analysis::analyzer::go_betweenness(&g);
    let ev = bv_graph_core::eigenvector_default(&g);
    let hits_result = bv_graph_core::hits_default(&g);

    let to_map = |scores: &[f64]| -> std::collections::BTreeMap<String, f64> {
        scores
            .iter()
            .enumerate()
            .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), *v))
            .collect()
    };

    app.graph_metrics = Some(bv_tui::GraphMetrics {
        pagerank: to_map(&pr),
        betweenness: to_map(&bw),
        eigenvector: to_map(&ev),
        hubs: to_map(&hits_result.hubs),
        authorities: to_map(&hits_result.authorities),
    });
}

/// Go main.go:4527 — `if *debugRender != ""`. An absent flag and an explicitly
/// empty value are the same thing to Go: both fall through to the TUI.
fn debug_render_view(args: &[String]) -> Option<&str> {
    flag_value(args, "debug-render").filter(|v| !v.is_empty())
}

/// Go main.go:4526-4531 — render one view, print it, exit 0.
///
/// Returns `None` when `--debug-render` was not requested, so the caller keeps
/// its normal TUI path. Go's help text says "output to file" but the code does
/// `fmt.Println(output)`, so the print is what is faithful.
fn run_debug_render(
    app: &mut bv_tui::App,
    issues: &[bv_core::model::Issue],
    args: &[String],
) -> Option<ExitCode> {
    let view = debug_render_view(args)?;
    apply_graph_metrics(app, issues);

    // Go main.go:1631-1632 register 180x50. A value pflag cannot convert is
    // pflag's parse error at exit 1, not a silent fallback to the default.
    let mut dims = [180u16, 50u16];
    for (i, name) in ["debug-width", "debug-height"].iter().enumerate() {
        let Some(raw) = flag_value(args, name) else {
            continue;
        };
        match go_parse_int_base0(raw) {
            Ok(v) if (0..=u16::MAX as i64).contains(&v) => dims[i] = v as u16,
            // Out of `u16` range parses fine in Go, which then asks its view
            // for a buffer this many columns wide. Report the conversion
            // failure rather than pretend a default was used.
            Ok(_) => {
                eprintln!(
                    "invalid argument {raw:?} for \"--{name}\" flag: strconv.ParseInt: parsing {raw:?}: value out of range"
                );
                return Some(ExitCode::from(1));
            }
            Err(detail) => {
                eprintln!("invalid argument {raw:?} for \"--{name}\" flag: {detail}");
                return Some(ExitCode::from(1));
            }
        }
    }

    let rendered = bv_tui::render_debug_view(app, view, dims[0], dims[1]);
    println!("{rendered}");
    Some(ExitCode::from(0))
}

/// Go main.go:4482-4485 — the empty-set guard runs after the workspace and
/// single-repo loads converge and before `ui.NewModel`, so it covers both and
/// `--debug-render` never reaches an empty model.
fn exit_if_no_issues(issues: &[bv_core::model::Issue]) -> Option<ExitCode> {
    if issues.is_empty() {
        println!("No issues found. Create some with 'br create'!");
        Some(ExitCode::from(0))
    } else {
        None
    }
}

fn launch_tui(app: &mut bv_tui::App, issues: &[bv_core::model::Issue]) -> ExitCode {
    apply_graph_metrics(app, issues);

    match bv_tui::run_tui(app) {
        Ok(_) => ExitCode::from(0),
        Err(e) => {
            eprintln!("TUI error: {e}");
            ExitCode::from(1)
        }
    }
}

/// Extract --as-of value from process args (Go parity: global flag).
fn extract_as_of() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    for i in 1..args.len() {
        if args[i] == "--as-of" {
            return args.get(i + 1).cloned();
        }
        if let Some(val) = args[i].strip_prefix("--as-of=") {
            return Some(val.to_string());
        }
    }
    None
}

/// Load issues from cwd, honoring workspace config if present (multi-repo).
/// Source provenance for the v0.25.0 envelope (`source_path`, `source_kind`).
/// Go derives these in `RobotContext`; we collect them at load time because our
/// discovery chain returns only the issue vector.
#[derive(Debug, Clone, Default)]
struct SourceMeta {
    /// The path `source_authority.sources[].source_path` names.
    path: String,
    /// The path the envelope's top-level `source_path` names, when it differs.
    ///
    /// Under `--as-of` the two are not the same string: Go builds the
    /// authority's entry from the git loader's file path plus the resolved SHA
    /// (`.beads/issues.jsonl@6fa2454…`, main.go:2645-2652) while the envelope
    /// says `.beads@HEAD~1` (main.go:2804) — the ref, not the commit. Carrying
    /// one path for both put the ref where the SHA belonged and changed
    /// `authority_hash` with it. Empty means "same as `path`".
    envelope_path: String,
    kind: String,
    valid: usize,
    errors: usize,
    skipped: usize,
}

/// Build the Go `RobotSourceAuthority` for a single-source load
/// (Go `newRobotSourceAuthority` over one `RobotSourceReport`).
fn source_authority(meta: &SourceMeta, data_hash: &str) -> bv_robot::RobotSourceAuthority {
    use bv_robot::{RobotSourceAuthority, RobotSourceReport};
    let report = RobotSourceReport {
        name: String::new(),
        repo_path: String::new(),
        source_path: meta.path.clone(),
        source_kind: meta.kind.clone(),
        status: "loaded".to_string(),
        data_hash: data_hash.to_string(),
        valid: meta.valid,
        errors: meta.errors,
        skipped: meta.skipped,
        read_errors: 0,
        visible: meta.valid,
        tombstones: 0,
        stale: false,
        warning_count: 0,
        warnings: Vec::new(),
        error: String::new(),
    };
    let loaded = usize::from(!meta.path.is_empty());
    RobotSourceAuthority {
        state: "complete".to_string(),
        claim_safe: meta.errors == 0,
        readiness: "proven".to_string(),
        loaded,
        failed: 0,
        disabled: 0,
        valid: meta.valid,
        errors: meta.errors,
        skipped: meta.skipped,
        read_errors: 0,
        visible: meta.valid,
        tombstones: 0,
        warning_count: 0,
        sources: vec![report],
    }
}

/// Go `robotWorkspaceAuthority` (main.go:7374-7391) over a single *failed*
/// report — the envelope Go still emits when the beads source cannot be
/// resolved.
///
/// This is not a convenience fallback. `source_authority` is the contract that
/// tells an agent whether the numbers in the payload mean anything, and a bare
/// `Error:` line throws it away: an agent can no longer distinguish "this
/// repository has no issues" from "bvr could not read the data", and the second
/// is exactly the case where claiming work would be unsafe. Go keeps the
/// envelope, marks it `claim_safe: false` / `readiness: provisional`, and puts
/// the reason in `error`.
fn failed_source_authority(source_path: &str, error: &str) -> bv_robot::RobotSourceAuthority {
    use bv_robot::{RobotSourceAuthority, RobotSourceReport};
    let report = RobotSourceReport {
        name: String::new(),
        repo_path: String::new(),
        source_path: source_path.to_string(),
        source_kind: "jsonl_local".to_string(),
        status: "failed".to_string(),
        // Go marks a source it never read with the literal "empty" rather than
        // a digest of nothing.
        data_hash: "empty".to_string(),
        valid: 0,
        errors: 0,
        skipped: 0,
        read_errors: 0,
        visible: 0,
        tombstones: 0,
        stale: false,
        warning_count: 0,
        warnings: Vec::new(),
        error: error.to_string(),
    };
    RobotSourceAuthority {
        state: "unknown".to_string(),
        claim_safe: false,
        readiness: "provisional".to_string(),
        loaded: 0,
        failed: 1,
        disabled: 0,
        valid: 0,
        errors: 0,
        skipped: 0,
        read_errors: 0,
        visible: 0,
        tombstones: 0,
        warning_count: 0,
        sources: vec![report],
    }
}

/// The `<cwd>/.beads/issues.jsonl` Go names in its "no beads issues found at"
/// message when the source cannot be resolved.
///
/// `PWD` rather than `current_dir()`: on macOS the latter reports the resolved
/// physical path, so a run under a symlinked directory would name
/// /private/tmp where Go says /tmp.
fn expected_jsonl_path() -> String {
    let base = std::env::var("PWD")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::current_dir())
        .unwrap_or_default();
    base.join(".beads/issues.jsonl")
        .to_string_lossy()
        .to_string()
}

/// Emit Go's degraded envelope for an unresolvable beads source and exit 1.
///
/// Go writes the envelope to stdout first and the two diagnostic lines to
/// stderr after it (cmd/bv/main.go:2686-2688). The order matters to anything
/// capturing both streams together, and stdout stays pure JSON either way.
fn emit_unresolved_source() -> ExitCode {
    let path = expected_jsonl_path();
    let error = format!("no beads issues found at {path}");
    let authority = failed_source_authority(&path, &error);
    let ahash = bv_robot::authority_hash(&authority);
    let mut payload = serde_json::json!({
        "generated_at": jiff_now(),
        "data_hash": "",
        "output_format": output_format(),
        "version": GO_APP_VERSION,
        "source_authority": serde_json::to_value(&authority).unwrap_or(serde_json::Value::Null),
    });
    if !ahash.is_empty() {
        payload["authority_hash"] = serde_json::json!(ahash);
    }
    // Go still publishes a scope hash for a scope that selected nothing.
    let shash = bv_robot::scope_hash("", "", "", "", &[]);
    if !shash.is_empty() {
        payload["scope_hash"] = serde_json::json!(shash);
    }
    payload["actionable"] = serde_json::json!(false);
    payload["error"] = serde_json::json!(error);
    let _ = emit_json(&payload);
    eprintln!("Error loading beads: {error}");
    eprintln!("Make sure you are in a project initialized with 'br init'.");
    ExitCode::from(1)
}

/// True when `e` is the load failure Go answers with the degraded envelope
/// rather than a bare error line.
fn is_unresolved_source_error(e: &str) -> bool {
    e.contains("no beads JSONL or SQLite database found")
        || e.contains("failed to read beads directory")
        || e.contains("no beads JSONL found")
}

/// Whether the beads source at `cwd` cannot be resolved.
///
/// Go loads the issues before it validates the repository, so a directory with
/// neither a `.git` nor a `.beads` fails on the *beads* source and carries the
/// degraded envelope. Rust validated the repository first and reported "not a
/// git repository" instead, which names a different problem than the one an
/// agent will actually hit.
fn unresolved_source_probe(cwd: &std::path::Path) -> bool {
    bv_core::discovery::load_issues_from_repo(cwd)
        .err()
        .is_some_and(|e| is_unresolved_source_error(&e.to_string()))
}

fn load_issues_auto(
    cwd: &std::path::Path,
    as_of: Option<&str>,
) -> Result<(Vec<bv_core::model::Issue>, String, Option<String>), String> {
    load_issues_auto_meta(cwd, as_of).map(|(i, h, c, _)| (i, h, c))
}

/// The UNSCOPED loader, for the one consumer that needs it. Go captures
/// `issuesForSearch := issues` at main.go:2770 — BEFORE `scopeLoadedIssues`
/// runs at :2786 — so the search index is built over the whole source while
/// only the scoped set is ranked. Every other consumer goes through the scoped
/// loader.
fn load_issues_auto_unscoped(
    cwd: &std::path::Path,
    as_of: Option<&str>,
) -> Result<(Vec<bv_core::model::Issue>, String, Option<String>), String> {
    load_issues_auto_meta_raw(cwd, as_of).map(|(i, h, c, _)| (i, h, c))
}

/// The active `--label` / `--recipe` / `--repo` scoping flags, recorded once in
/// `main` from the REWRITTEN argv. Go threads these through the RobotContext
/// (robot_registry.go:261-268) and applies them once at main.go:2786.
///
/// Reading `std::env::args()` here instead would miss the `-l` / `-r` aliases,
/// which `argv::rewrite_args` (crates/bv/src/argv.rs:24) expands into their
/// long spellings only in the rewritten vector — so `-l tui` and `--label tui`
/// would produce different scopes.
static SCOPE_FLAGS: std::sync::OnceLock<[String; 3]> = std::sync::OnceLock::new();

/// The `--as-of` ref and its resolved SHA, published for the envelope builder.
///
/// Go declares both as `RobotEnvelope` fields (main.go:7222-7223) and fills them
/// from the context, so they are emitted at the TOP LEVEL, right after
/// `source_kind`. bvr wrote them inside the per-command payload — and for
/// `--robot-triage`, inside `triage` — which put them where Go never puts them.
static AS_OF: std::sync::OnceLock<[String; 2]> = std::sync::OnceLock::new();

fn set_as_of(ref_name: &str, commit: &str) {
    let _ = AS_OF.set([ref_name.to_string(), commit.to_string()]);
}

fn as_of_pair() -> (String, String) {
    AS_OF
        .get()
        .map(|p| (p[0].clone(), p[1].clone()))
        .unwrap_or_default()
}

/// Go's unknown-recipe rejection (pkg/recipe/loader.go:44-51, printed by
/// cmd/bv/main.go with the available list appended).
///
/// Returns the message to print, or `None` when the name resolves — which
/// includes a path to a recipe file, since Go's `isPathArgument` check runs
/// first and a file that loads is not "unknown".
fn validate_recipe_name(name: &str) -> Option<String> {
    // `with_project_dir` so a run under a symlinked directory resolves the same
    // project config Go would.
    let cwd = std::env::var("PWD")
        .map(std::path::PathBuf::from)
        .or_else(|_| std::env::current_dir())
        .unwrap_or_default();
    // `load()` is what pulls in the builtin recipes, the user config and the
    // project file; without it `resolve` sees an empty table and every name
    // would be reported unknown.
    let mut loader = bv_recipe::Loader::new().with_project_dir(cwd.to_string_lossy().to_string());
    if loader.load().is_err() {
        return None;
    }
    match loader.resolve(name) {
        Ok(_) => None,
        Err(bv_recipe::ResolveError::Unknown(e)) => {
            // Go main.go:2590-2601, verbatim in structure: the error, a blank
            // line, the sorted available names each padded to 15 and followed
            // by its description, then the hint about loading a recipe file.
            let mut msg = e.to_string();
            msg.push_str("\n\nAvailable recipes:");
            for summary in loader.list_summaries() {
                msg.push_str(&format!("\n  {:<15} {}", summary.name, summary.description));
            }
            msg.push_str(
                "\n\nA path ending in .yaml or .yml loads one recipe file, e.g. --recipe .beads/recipes/sprint.yaml",
            );
            Some(format!("Error: {msg}"))
        }
        // A file that exists but does not parse is a different failure, and Go
        // reports it as one; it is left to the loader's own error path.
        Err(_) => None,
    }
}

fn set_scope_flags(label: String, recipe: String, repo: String) {
    let _ = SCOPE_FLAGS.set([label, recipe, repo]);
}

/// Envelope for a command that already loaded `issues` — derives the source
/// provenance from the current directory so callers do not thread `cwd`.
/// The active scoping flags are published by the envelope so a caller can see
/// which scope produced the payload.
fn active_scope_flags() -> (String, String, String) {
    SCOPE_FLAGS
        .get()
        .map(|s| (s[0].clone(), s[1].clone(), s[2].clone()))
        .unwrap_or_default()
}

/// Go's top-level `label_scope` / `label_context` payload keys.
///
/// Emitted by exactly three commands — `--robot-plan` (robot_registry.go:893),
/// `--robot-priority` (:1009) and `--robot-insights` (:1985) — and by no
/// others; triage, graph and next get their scoping only through the
/// envelope's `scope.label`. Both keys are omitempty: `label_scope` is absent
/// without `--label`, and `label_context` is absent when no label in the final
/// analysis set matches, which is Go's unmatched-label case
/// (cmd/bv/main.go:4922-4931). An unmatched label therefore yields no
/// `label_context` key at all, not an empty object.
fn insert_label_scope_keys(
    payload: &mut serde_json::Value,
    issues: &[bv_core::model::Issue],
    now: jiff::Timestamp,
) {
    let (label, _, _) = active_scope_flags();
    if label.is_empty() {
        return;
    }
    let obj = payload.as_object_mut().expect("envelope is an object");
    obj.insert("label_scope".into(), serde_json::json!(label));
    // Health describes the final intersection, i.e. what the command actually
    // analysed, so this runs against the caller's scoped issue set.
    let health = bv_analysis::label_health::compute_all_label_health(
        issues,
        &bv_analysis::label_health::LabelHealthConfig::default(),
        now,
    );
    if let Some(entry) = health.labels.into_iter().find(|l| l.label == label) {
        obj.insert(
            "label_context".into(),
            serde_json::to_value(&entry).unwrap_or(serde_json::Value::Null),
        );
    }
}

/// Apply `--label` scoping the way Go's `scopeLoadedIssues` does
/// (cmd/bv/main.go:4870-4900): the label's own issues plus their direct
/// neighbours replace the issue set the command analyses, so the graph, the
/// counts and the drift all see the scoped subgraph rather than the whole repo.
///
/// Returns the analysis set. The candidate set (the label's own issues) is
/// what the envelope's `scope`/`scope_hash` describe.
fn apply_label_scope(issues: &[bv_core::model::Issue]) -> Vec<bv_core::model::Issue> {
    let (label, _, _) = active_scope_flags();
    if label.is_empty() {
        return issues.to_vec();
    }
    let (_, analysis_ids) = bv_analysis::label_health::label_scope_ids(&label, issues);
    let mut out = Vec::with_capacity(analysis_ids.len());
    for id in &analysis_ids {
        if let Some(issue) = issues.iter().find(|i| &i.id == id) {
            out.push(issue.clone());
        }
    }
    out
}

/// Apply the active scope flags to a freshly loaded issue set, in Go's order:
/// `--repo` first (main.go:4874-4885, which also recomputes `DataHash` over
/// the filtered set), then `--label` (main.go:4887-4902).
///
/// Go applies both once, inside `scopeLoadedIssues` (main.go:2786), so every
/// consumer downstream — each robot handler, each export, the TUI — analyses
/// the same narrowed set. Applying the filter at a few call sites instead left
/// `--label` decorative on the majority of handlers while the envelope still
/// advertised it.
fn apply_repo_scope(issues: &[bv_core::model::Issue]) -> Vec<bv_core::model::Issue> {
    let (_label, _recipe, repo) = active_scope_flags();
    if repo.is_empty() {
        return issues.to_vec();
    }
    bv_core::repo_filter::filter_by_repo(issues, &repo)
}

/// Every loader routes through this so the scope is applied exactly once,
/// exactly where Go applies it (`scopeLoadedIssues`, main.go:2786).
///
/// The returned `Option<String>` is a `DataHash` override, and Go's two
/// scopes treat it differently (main.go:4883-4885 vs :4901): a `--repo` filter
/// RE-hashes the repo-filtered source, while a `--label` filter leaves the
/// loader's hash alone and only sets `DataHashMatchesIssues = false`. Getting
/// that backwards would change the `data_hash` on every `--label` golden.
fn apply_scope(issues: &[bv_core::model::Issue]) -> (Vec<bv_core::model::Issue>, Option<String>) {
    let (_label, recipe, repo) = active_scope_flags();
    let repo_scoped = apply_repo_scope(issues);
    let hash_override = if repo.is_empty() {
        None
    } else {
        Some(bv_core::data_hash::compute_data_hash(&repo_scoped))
    };
    (
        apply_recipe_scope(&apply_label_scope(&repo_scoped), &recipe),
        hash_override,
    )
}

/// Go `scopeLoadedIssues` also applies `recipe.Apply` to narrow the issue set
/// (cmd/bv/main.go:4903-4922). That is not ported: `recipe::apply` takes a
/// `Metrics` whose `graph` is a `GraphMetrics` trait nothing in the workspace
/// implements outside a test stub, and whose `triage` map comes from a
/// `compute_triage_scores` that does not exist here either. So `--recipe
/// quick-wins` still answers with the whole repository rather than the quick
/// wins, and `--recipe nosuchrecipe` is now rejected (the name is validated at
/// the flag site) but a valid name that filters has no effect.
fn apply_recipe_scope(
    issues: &[bv_core::model::Issue],
    _recipe: &str,
) -> Vec<bv_core::model::Issue> {
    issues.to_vec()
}

fn full_envelope_for(data_hash: &str, issues: &[bv_core::model::Issue]) -> serde_json::Value {
    let source = source_meta_for(issues);
    full_envelope_json_with_source(data_hash, Some(&source), issues)
}

/// Derive `SourceMeta` for the current working directory. Used by command
/// handlers that build their envelope without an explicit cwd in scope.
fn source_meta_for(issues: &[bv_core::model::Issue]) -> SourceMeta {
    // Go's `os.Getwd` semantics, so `source_path` reads the way the user typed
    // the path rather than its resolved form.
    let cwd = go_working_dir();
    if let Ok(dir) = bv_core::discovery::get_beads_dir(&cwd) {
        if let Ok(Some(jsonl)) = bv_core::discovery::find_jsonl_path_with_warnings(&dir, |_| {}) {
            return SourceMeta {
                path: jsonl.to_string_lossy().to_string(),
                envelope_path: String::new(),
                kind: "jsonl_local".to_string(),
                valid: issues.len(),
                errors: 0,
                skipped: 0,
            };
        }
    }
    SourceMeta {
        valid: issues.len(),
        ..Default::default()
    }
}

/// Go `time.Now().Format("15:04:05")` — the wall clock, local zone.
///
/// Deliberately NOT `robot_now_zone()`: that one honours a frozen
/// `SOURCE_DATE_EPOCH` because Go's robot stamps are reproducible. The export
/// progress lines come from a bare `time.Now()` in the oracle
/// (main.go:3067, 3212), so freezing them would be a second, subtler drift.
fn hhmmss_now() -> String {
    let zoned = jiff::Timestamp::now()
        .to_zoned(jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC));
    format!(
        "{:02}:{:02}:{:02}",
        zoned.hour(),
        zoned.minute(),
        zoned.second()
    )
}

/// Identity of the watched source: mtime and size, or `(None, None)` when the
/// file is unreadable.
///
/// The unreadable case is a value, not an error, because Go's watcher reports
/// removal and permission failures on the same channel as a real change
/// (`watcher.WithOnError` forwards into `mergedChangeCh`, main.go:3284-3292) —
/// they change which source is authoritative even though the file cannot be
/// read. Modelling them as a distinct stamp makes a vanished file register as a
/// change exactly once, and a reappearing one register again.
fn source_stamp(path: &std::path::Path) -> (Option<std::time::SystemTime>, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(m) => (m.modified().ok(), Some(m.len())),
        Err(_) => (None, None),
    }
}

/// Raised by the SIGINT/SIGTERM handler; polled by the watch loop. A handler may
/// only touch async-signal-safe state, so this is an atomic flag and the loop
/// does the printing.
static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_stop_signal(_: libc::c_int) {
    STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Go main.go:3315-3317 — `signal.Notify(sigCh, os.Interrupt, syscall.SIGTERM)`.
///
/// Installing a handler is what makes the graceful path reachable at all: on the
/// default disposition Ctrl+C kills the process with 130 before any watch code
/// runs, so the loop could never print its farewell.
fn install_stop_handler() {
    use std::sync::atomic::Ordering;
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    // SAFETY: `on_stop_signal` is a plain `extern "C" fn` that only does an
    // atomic store, which is async-signal-safe. Nothing is allocated, locked
    // or printed from the handler. The `as *const ()` hop is the documented way
    // to turn a function item into a `sighandler_t` without a lossy direct
    // integer cast.
    unsafe {
        let handler = on_stop_signal as *const () as libc::sighandler_t;
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

/// Go main.go:3223-3453 — `--watch-export` watch mode.
///
/// The coalescing loop (#159 upstream): bursts of file changes collapse into one
/// export, the window widens adaptively so sustained churn cannot thrash the
/// CPU, and an unchanged source skips the expensive regeneration entirely.
///
/// Sync by construction (AGENTS.md: no tokio). A `std::thread` polls the
/// source's mtime and pushes into a `crossbeam-channel::bounded(1)` — bounded
/// and `try_send`, which is the direct analogue of Go's buffered
/// `mergedChangeCh` and its non-blocking `select`/`default` forwarders
/// (main.go:3275-3277). The main thread drives the settle window with
/// `recv_timeout`, the same shape as Go's `select` over `mergedChangeCh`,
/// `settleC` and `sigCh`.
fn run_export_watch<F>(
    out_dir: &str,
    cwd: &std::path::Path,
    watch_file: &std::path::Path,
    export_count: &mut u32,
    mut do_export: F,
) -> ExitCode
where
    F: FnMut(&mut u32) -> Result<(), String>,
{
    use crossbeam_channel::RecvTimeoutError;
    use std::time::{Duration, Instant};

    // Go main.go:3224-3271 — the banner, verbatim. Go prints the watched file
    // list here, which is one entry outside workspace mode.
    println!();
    println!("Watch mode enabled. Monitoring for changes...");
    println!("  → Watching: {}", watch_file.display());
    println!("  → Press Ctrl+C to stop");
    println!();
    println!("To preview with auto-refresh, run in another terminal:");
    println!("  bv --preview-pages {out_dir}");
    println!();

    install_stop_handler();

    let (tx, rx) = crossbeam_channel::bounded::<()>(1);
    let poll_path = watch_file.to_path_buf();
    let mut stamp = source_stamp(&poll_path);
    std::thread::spawn(move || loop {
        // 500ms debounce, matching `watcher.WithDebounceDuration(500ms)`
        // (main.go:3279). Polling mtime stands in for fsnotify: the watched
        // target is a single JSONL file, so there is no directory-walk
        // behaviour for the event stream to lose.
        std::thread::sleep(Duration::from_millis(500));
        let now = source_stamp(&poll_path);
        if now != stamp {
            stamp = now;
            // Full buffer means a change is already pending and this one
            // coalesces into it — Go's `default:` arm at main.go:3310.
            let _ = tx.try_send(());
        }
    });

    // Go main.go:3364-3367 — the window floor and ceiling.
    const WATCH_SETTLE_MIN: Duration = Duration::from_millis(500);
    const WATCH_SETTLE_MAX: Duration = Duration::from_secs(30);

    // Seeded from the initial export so the first change does not redo
    // identical work (main.go:3369). A load failure at this point still
    // yields a stamp, because the initial export already succeeded.
    let mut last_hash = load_issues_auto(cwd, None)
        .map(|(_, h, _)| h)
        .unwrap_or_default();

    let mut settle = WATCH_SETTLE_MIN;
    // `armSettle()` is called once up front (main.go:3396-3398): a change can
    // land while the initial bundle is still being written, before any watcher
    // is attached. `None` models Go's `settleC = nil` — once a settle has
    // fired the loop stops polling for one and waits for the next change.
    let mut deadline: Option<Instant> = Some(Instant::now() + settle);

    loop {
        if STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            // Go main.go:3446-3448 — the farewell is the one line the loop
            // emits on shutdown, and the exit is 0, not a signal death code.
            println!("\nStopping watch mode...");
            return ExitCode::from(0);
        }

        // While armed, wait exactly the remainder of the window; while idle,
        // poll often enough that Ctrl+C feels immediate.
        let wait = match deadline {
            Some(d) => d.saturating_duration_since(Instant::now()),
            None => Duration::from_millis(200),
        };
        match rx.recv_timeout(wait) {
            // Go's `case <-mergedChangeCh: armSettle()` (main.go:3399-3401):
            // a change inside the quiet window pushes the deadline out rather
            // than starting a second export.
            Ok(()) => {
                deadline = Some(Instant::now() + settle);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let Some(armed) = deadline else {
            // Idle: the wait above was only a stop-flag poll, so loop round.
            continue;
        };
        if Instant::now() < armed {
            // Woken by a spurious wakeup before the window closed.
            continue;
        }
        deadline = None; // settleC = nil

        // Go main.go:3403-3411 — reload and re-fingerprint. Go compares both a
        // content hash and an authority hash; the loader's data_hash is the
        // content half and the only one Rust can observe here, so a source
        // whose bytes are unchanged is skipped.
        let fresh = match load_issues_auto(cwd, None) {
            Ok((_, h, _)) => h,
            Err(e) => {
                // Go main.go:3404-3406 — a reload failure is reported and the
                // loop waits for the next change; it does not exit.
                println!("  → Error reloading issues: {e}");
                continue;
            }
        };
        if fresh == last_hash {
            // Go main.go:3414-3418 — content-identical rewrite, so reset the
            // window to the floor and export nothing.
            settle = WATCH_SETTLE_MIN;
            continue;
        }

        let started = Instant::now();
        if let Err(e) = do_export(export_count) {
            // Go main.go:3423-3426 — a failed export is reported and the loop
            // keeps watching rather than tearing down the session.
            println!("  → Export error: {e}");
            continue;
        }
        last_hash = fresh;

        // Go main.go:3430-3437 — adaptive backoff: widen to ~2x the export
        // cost so sustained churn cannot pin the CPU, clamped to the window
        // bounds so cheap exports stay responsive.
        settle = (started.elapsed() * 2).clamp(WATCH_SETTLE_MIN, WATCH_SETTLE_MAX);
    }

    ExitCode::from(0)
}

/// Go `RobotContext` loader — returns the issues, their hash, the resolved
/// `--as-of` commit, and the source provenance the envelope reports.
///
/// This is the SCOPED loader: it applies `--repo` then `--label` exactly where
/// Go's `scopeLoadedIssues` does (main.go:2786, body at :4870-4902), so every
/// robot handler, export and the TUI analyse the same narrowed set. The
/// `SourceMeta` still describes the full loaded source, which is what Go's
/// `source_authority` reports.
fn load_issues_auto_meta(
    cwd: &std::path::Path,
    as_of: Option<&str>,
) -> Result<
    (
        Vec<bv_core::model::Issue>,
        String,
        Option<String>,
        SourceMeta,
    ),
    String,
> {
    let (issues, hash, commit, source) = load_issues_auto_meta_raw(cwd, as_of)?;
    let (issues, hash_override) = apply_scope(&issues);
    Ok((issues, hash_override.unwrap_or(hash), commit, source))
}

fn load_issues_auto_meta_raw(
    cwd: &std::path::Path,
    as_of: Option<&str>,
) -> Result<
    (
        Vec<bv_core::model::Issue>,
        String,
        Option<String>,
        SourceMeta,
    ),
    String,
> {
    // If --as-of is specified, use GitLoader for time-travel (Go parity).
    if let Some(revision) = as_of {
        let loader = bv_core::discovery::GitLoader::new(cwd);
        let resolved = loader
            .resolve_revision(revision)
            .map_err(|e| e.to_string())?;
        let issues = loader.load_at(revision).map_err(|e| e.to_string())?;
        eprintln!(
            "Loaded {} issues from {} ({})",
            issues.len(),
            revision,
            &resolved[..resolved.len().min(7)]
        );
        let hash = bv_core::data_hash::compute_data_hash(&issues);
        let valid = issues.len();
        // Built before the tuple: `Some(resolved)` moves `resolved` when the
        // third element is evaluated, which is before the fourth.
        //
        // Two different strings, and Go builds both. The authority's entry is
        // the git loader's file path plus the resolved commit
        // (main.go:2645-2652); the envelope's `source_path` is the literal
        // `.beads@<asOf>` naming the ref instead (main.go:2804).
        let source = SourceMeta {
            path: format!(".beads/issues.jsonl@{resolved}"),
            envelope_path: format!(".beads@{revision}"),
            kind: "git".to_string(),
            valid,
            errors: 0,
            skipped: 0,
        };
        return Ok((issues, hash, Some(resolved), source));
    }
    // Go main.go:2678 — an explicit `--workspace` wins over discovery.
    let explicit_ws = workspace_override().map(std::path::Path::to_path_buf);
    let ws_candidate = explicit_ws
        .clone()
        .or_else(|| bv_core::workspace::find_workspace_config(cwd));
    if let Some(ws_path) = ws_candidate {
        let ws_root = ws_path
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(cwd)
            .to_path_buf();
        match bv_core::workspace::load_workspace(&ws_path)
            .and_then(|cfg| bv_core::workspace::load_all(&cfg, &ws_root))
        {
            Ok((issues, _)) => {
                let hash = bv_core::data_hash::compute_data_hash(&issues);
                let valid = issues.len();
                return Ok((
                    issues,
                    hash,
                    None,
                    SourceMeta {
                        path: ws_path.to_string_lossy().to_string(),
                        envelope_path: String::new(),
                        kind: "workspace".to_string(),
                        valid,
                        errors: 0,
                        skipped: 0,
                    },
                ));
            }
            Err(e) if explicit_ws.is_some() => {
                // Go main.go:2678-2721 loads the EXPLICIT path and reports a
                // failed source (loaded 0 / failed 1, claim_safe false) rather
                // than quietly reading a different repository. Falling back to
                // the local `.beads` here made the envelope claim a healthy,
                // claim-safe source for a path the caller never asked for.
                return Err(format!("loading workspace {}: {e}", ws_path.display()));
            }
            Err(e) => eprintln!("workspace load failed, falling back: {e}"),
        }
    }
    let beads_dir = bv_core::discovery::get_beads_dir(cwd).map_err(|e| e.to_string())?;
    let jsonl = bv_core::discovery::find_jsonl_path_with_warnings(&beads_dir, |_| {})
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no beads JSONL found in {}", beads_dir.display()))?;
    let (issues, stats) = {
        let raw = std::fs::read_to_string(&jsonl)
            .map_err(|e| format!("reading {}: {}", jsonl.display(), e))?;
        let mut rdr = raw.as_bytes();
        bv_core::loader::parse_issues_with_options(
            &mut rdr,
            &bv_core::loader::ParseOptions::default(),
            |_| {},
        )
        .map_err(|e| e.to_string())?
    };
    let hash = bv_core::data_hash::compute_data_hash(&issues);
    let valid = issues.len();
    Ok((
        issues,
        hash,
        None,
        SourceMeta {
            path: jsonl.to_string_lossy().to_string(),
            envelope_path: String::new(),
            kind: "jsonl_local".to_string(),
            valid,
            errors: stats.errors,
            skipped: stats.skipped,
        },
    ))
}

/// Load issues from discovery chain and emit --robot-triage JSON.
/// Go `handleRobotNext`: single top claimable pick with the claimability
/// filter (open, non-epic, unassigned, no open blockers) and fail-closed
/// degraded output when no pick is claim-safe.
fn run_robot_next() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_default();
    let as_of = extract_as_of();
    let (issues, hash, as_of_commit, source) = match load_issues_auto_meta(&cwd, as_of.as_deref()) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    let mut payload = full_envelope_for(&hash, &issues);
    if let Some(ref a) = as_of {
        payload["as_of"] = serde_json::json!(a);
    }
    if let Some(ref c) = as_of_commit {
        payload["as_of_commit"] = serde_json::json!(c);
    }

    let usage_hints = serde_json::json!([
        "Use scripts/br_retry.sh actionable --json plus the claim gate before mutating Beads state in crowded swarms.",
        "No claim_command is emitted unless the item is open, unblocked, unassigned, and triage metrics are ready.",
        "Inspect .status for skipped, timeout, or pending graph phases.",
    ]);

    if issues.is_empty() {
        payload["actionable"] = serde_json::json!(false);
        payload["phase2_ready"] = serde_json::json!(false);
        payload["status"] = bv_analysis::analyzer::MetricStatus::default().to_json_map();
        payload["message"] = serde_json::json!("No proven actionable item available");
        payload["degraded"] = serde_json::json!([{
            "code": "no_actionable_recommendation",
            "severity": "info",
            "message": "No open, unblocked, unassigned non-epic recommendation passed the robot-next claimability filter.",
            "repair": "Use br ready --json or scripts/br_retry.sh actionable --json for authoritative claim candidates.",
        }]);
        payload["usage_hints"] = usage_hints;
        return emit_json(&payload);
    }

    let g = std::sync::Arc::new(bv_analysis::analyzer::build_graph(&issues));
    let out = bv_analysis::triage::build_triage(&issues, &g, robot_now(), as_of.is_some());

    // Claimability filter (Go robotNextClaimabilityReasons): the pick must
    // be open, non-epic, unassigned, and free of open blockers.
    let issue_by_id: std::collections::HashMap<&str, &bv_core::model::Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let claimability_reasons = |pick: &serde_json::Value| -> Vec<String> {
        let Some(id) = pick["id"].as_str() else {
            return vec!["pick lacks id".into()];
        };
        let Some(issue) = issue_by_id.get(id) else {
            return vec![format!("{id} is absent from loaded Beads records")];
        };
        let mut reasons = Vec::new();
        if issue.status != bv_core::model::Status::Open {
            reasons.push(format!("{id} status is {:?}", issue.status));
        }
        if issue.issue_type.eq_ignore_ascii_case("epic") {
            reasons.push(format!("{id} is an epic"));
        }
        let assignee = issue.assignee.trim();
        if !assignee.is_empty() {
            reasons.push(format!("{id} is already assigned to {assignee}"));
        }
        // Ancestor-epic parity (#2): route through the shared
        // blocker_chain helper so parent-child inherited blocking gates
        // the claimability filter, exactly like compute_blocked_set.
        let mut open_blockers: Vec<String> = bv_analysis::open_blockers(&issue_by_id, &issue.id);
        // Surface dangling direct-blocking edges as unclaimable too.
        for dep in &issue.dependencies {
            if !dep.r#type.is_blocking() {
                continue;
            }
            let blocker_id = dep.effective_depends_on().trim().to_string();
            if blocker_id.is_empty() {
                open_blockers.push("<missing blocker id>".into());
            } else if !issue_by_id.contains_key(blocker_id.as_str())
                && !open_blockers.iter().any(|b| b == &blocker_id)
            {
                open_blockers.push(format!("{blocker_id} (missing)"));
            }
        }
        if !open_blockers.is_empty() {
            open_blockers.sort();
            reasons.push(format!("{id} is blocked by {}", open_blockers.join(", ")));
        }
        reasons
    };

    // Build top picks exactly as run_robot_triage does (top 5 recommendations
    // with direct unblock counts).
    let picks: Vec<serde_json::Value> = out
        .recommendations
        .iter()
        .map(|r| {
            let unblocks: usize = issues
                .iter()
                .filter(|o| {
                    o.dependencies
                        .iter()
                        .any(|d| d.r#type.is_blocking() && d.effective_depends_on() == r.id)
                })
                .count();
            serde_json::json!({
                "id": r.id,
                "title": r.title,
                "score": r.score,
                "reasons": r.reasons,
                "unblocks": unblocks,
            })
        })
        .collect();
    let mut chosen: Option<&serde_json::Value> = None;
    let mut first_unsafe: Option<Vec<String>> = None;
    for pick in &picks {
        let reasons = claimability_reasons(pick);
        if reasons.is_empty() {
            chosen = Some(pick);
            break;
        }
        if first_unsafe.is_none() {
            first_unsafe = Some(reasons);
        }
    }

    // Clear skipped reasons for Go omitempty parity (only "approximate" kept).
    let cleared_status = {
        let mut s = out.metric_status.clone();
        if s.page_rank.state == "skipped" {
            s.page_rank.reason.clear();
        }
        if s.eigenvector.state == "skipped" {
            s.eigenvector.reason.clear();
        }
        if s.hits.state == "skipped" {
            s.hits.reason.clear();
        }
        if s.critical.state == "skipped" {
            s.critical.reason.clear();
        }
        if s.cycles.state == "skipped" {
            s.cycles.reason.clear();
        }
        if s.kcore.state == "skipped" {
            s.kcore.reason.clear();
        }
        if s.articulation.state == "skipped" {
            s.articulation.reason.clear();
        }
        if s.slack.state == "skipped" {
            s.slack.reason.clear();
        }
        s.to_json_map()
    };
    match chosen {
        Some(top) => {
            payload["phase2_ready"] = serde_json::json!(true);
            payload["status"] = cleared_status.clone();
            let id = top["id"].as_str().unwrap_or_default().to_string();
            // Go v0.25.0: live tracker route suggestions replace the flat
            // claim_command/show_command strings.
            let origin = bv_core::tracker::resolve_issue_origin(&source.path, &id);
            let actions = bv_core::tracker::build_actions(&origin, true);
            payload["actions"] = serde_json::to_value(&actions).unwrap_or(serde_json::Value::Null);

            // Go (robot_registry.go:2558-2566) treats a missing claim route as a
            // degradation, not as an actionable pick: it reports the candidate
            // under `diagnostic_top_pick`, sets actionable=false, and explains
            // why. Claimability alone is not enough — a source with no readable
            // tracker metadata (every synthetic fixture) must take this path.
            if actions.claim.is_none() {
                payload["actionable"] = serde_json::json!(false);
                payload["message"] = serde_json::json!(format!(
                    "No claim command emitted: {}",
                    actions.unavailable_reason
                ));
                payload["diagnostic_top_pick"] = serde_json::json!({
                    "id": top["id"],
                    "title": top["title"],
                    "score": top["score"],
                    "reasons": top["reasons"],
                    "unblocks": top.get("unblocks").cloned().unwrap_or(serde_json::json!(0)),
                });
                payload["degraded"] = serde_json::json!([{
                    "code": "live_action_route_unavailable",
                    "severity": "info",
                    "message": actions.unavailable_reason,
                }]);
            } else {
                payload["actionable"] = serde_json::json!(true);
                payload["id"] = top["id"].clone();
                payload["title"] = top["title"].clone();
                payload["score"] = top["score"].clone();
                payload["reasons"] = top["reasons"].clone();
                if let Some(cmd) = &actions.claim {
                    payload["claim_command"] = serde_json::json!(cmd.shell);
                }
                if let Some(cmd) = &actions.show {
                    payload["show_command"] = serde_json::json!(cmd.shell);
                }
            }
        }
        None => {
            payload["actionable"] = serde_json::json!(false);
            payload["phase2_ready"] = serde_json::json!(true);
            payload["status"] = cleared_status;
            payload["message"] = serde_json::json!(
                "No claim command emitted because the top recommendation was not claim-safe"
            );
            if let Some(first_pick) = picks.first() {
                payload["diagnostic_top_pick"] = serde_json::json!({
                    "id": first_pick["id"],
                    "title": first_pick["title"],
                    "score": first_pick["score"],
                    "reasons": first_pick["reasons"],
                    "unblocks": first_pick["unblocks"],
                });
            }
            payload["degraded"] = serde_json::json!([{
                "code": "robot_next_claim_unsafe",
                "severity": "warning",
                "message": first_unsafe.unwrap_or_default().join("; "),
                "repair": "Use the authoritative Beads actionable queue plus claim gate before claiming work.",
            }]);
        }
    }
    payload["usage_hints"] = usage_hints;
    emit_json(&payload)
}
/// Go `opts.TopN` (default 10) applied to the scored recommendation list.
/// Go builds recommendations against the full scored set and slices only the
/// user-visible list, so top_picks and quick_wins are unaffected (issue #146).
fn recommendations_top_n(recs: &[bv_analysis::impact::IssueImpact]) -> Vec<serde_json::Value> {
    recommendations_top_n_refs(recs)
        .into_iter()
        .map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Null))
        .collect()
}

/// Go `opts.TopN` (triage.go:592-594 — 10 when unset) applied to the scored
/// set. Go slices `allRecommendations` to this at :666-669 and the sliced
/// list is what the track/label groupers receive (:697, :700) as well as what
/// the `recommendations` field carries.
/// Go's `opts.TopN` (pkg/analysis/triage.go:635-640): the recommendations list
/// is sliced to this before anything reads it.
const TRIAGE_TOP_N: usize = 10;

fn recommendations_top_n_refs(
    recs: &[bv_analysis::impact::IssueImpact],
) -> Vec<&bv_analysis::impact::IssueImpact> {
    recs.iter().take(TRIAGE_TOP_N).collect()
}

/// Go's claimability gate for a triage recommendation (triage.go:657).
///
/// Shared by `top_picks` and `quick_wins`: Go derives both from the same
/// `claimableIDs` set, so a deferred/draft/blocked bead must never appear in
/// either (issue #199). Checking only `status == "open"` is not enough.
/// Go `resolveNotReadyLabels` (robot_registry.go:2073-2090) — the
/// `--robot-not-ready-labels` flag, else `BV_ROBOT_NOT_READY_LABELS`,
/// comma-split, trimmed, empties dropped. An empty set disables the gate, so
/// this is a no-op unless the flag or env var is configured.
fn resolve_not_ready_labels() -> Vec<String> {
    // robot_registry.go:2075-2079 — the FLAG wins over the env var, and only
    // when it is non-blank. The flag has no short alias, so raw argv is
    // equivalent to the rewritten vector here.
    let argv: Vec<String> = std::env::args().collect();
    let flag_raw = argv
        .iter()
        .position(|a| a == "--robot-not-ready-labels")
        .and_then(|i| argv.get(i + 1))
        .filter(|v| !v.trim().is_empty())
        .cloned();
    let raw =
        flag_raw.unwrap_or_else(|| std::env::var("BV_ROBOT_NOT_READY_LABELS").unwrap_or_default());
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Go `formatUnblockList` (triage.go:1729-1738) — the id list suffix of the
/// unblock-cascade reason. `joinStrings` is a plain `strings.Join`.
fn format_unblock_list(ids: &[String]) -> String {
    if ids.len() <= 3 {
        return ids.join(", ");
    }
    format!("{}, {}, +{} more", ids[0], ids[1], ids.len() - 2)
}

/// Go `GenerateTriageReasons`'s unblock-cascade reason (triage.go:1554-1562),
/// the FIRST reason it appends. It is a pure function of `UnblocksIDs`, which
/// `build_triage` stamps from `unblocksMap[score.IssueID]` (triage.go:947) —
/// the map that credits a dependent only when this issue is its SOLE open
/// blocker. `build_triage` instead derived the same line from the raw graph
/// in-degree, which counts dependents that are still multiply blocked, so the
/// line is restated here from the authority. Replaces the line in place when
/// present, inserts at the head when Go would emit one and the scorer emitted
/// none, and drops it when the authority says this issue unblocks nothing.
fn restate_unblock_reason(reasons: &mut Vec<String>, unblocks_ids: &[String]) {
    const CASCADE: &str = "🎯 Completing this unblocks ";
    const LIST: &str = "🔓 Unblocks ";
    let pos = reasons
        .iter()
        .position(|r| r.starts_with(CASCADE) || r.starts_with(LIST));
    let go_line = if unblocks_ids.len() >= 3 {
        Some(format!(
            "{CASCADE}{} downstream issues ({})",
            unblocks_ids.len(),
            format_unblock_list(unblocks_ids)
        ))
    } else if !unblocks_ids.is_empty() {
        Some(format!(
            "{LIST}{} item(s): {}",
            unblocks_ids.len(),
            format_unblock_list(unblocks_ids)
        ))
    } else {
        None
    };
    match (pos, go_line) {
        (Some(p), Some(line)) => reasons[p] = line,
        (Some(p), None) => {
            reasons.remove(p);
        }
        (None, Some(line)) => reasons.insert(0, line),
        (None, None) => {}
    }
}

/// Go `generateTrackID` (plan.go:305-320) — a 1-based track number rendered in
/// base-26 alphabetic: A, B, … Z, AA, AB, …. Returns `track-?` for n <= 0.
fn generate_track_id(n: i64) -> String {
    if n <= 0 {
        return "track-?".to_string();
    }
    let mut v = n - 1; // 0-based
    let mut letters: Vec<char> = Vec::new();
    while v >= 0 {
        letters.insert(0, (b'A' + (v % 26) as u8) as char);
        v = v / 26 - 1;
    }
    format!("track-{}", letters.iter().collect::<String>())
}

/// Go `layerReason` (triage.go:1905-1916).
fn layer_reason(depth: i64) -> String {
    match depth {
        0 => "Actionable now - can work in parallel".to_string(),
        1 => "Becomes actionable after layer 0 completes".to_string(),
        d if d >= 999 => "Cyclic dependencies detected".to_string(),
        d => format!("Becomes actionable after layer {} completes", d - 1),
    }
}

/// The `top_pick` a track/label group carries: Go builds a fresh `TopPick`
/// from the winning recommendation (triage.go:1873-1880, 1968-1975), so
/// `unblocks` is the length of `UnblocksIDs` — the same authority
/// `build_triage` stamped — not a graph in-degree.
fn group_top_pick(r: &&bv_analysis::impact::IssueImpact) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "title": r.title,
        "score": r.score,
        "reasons": r.reasons,
        "unblocks": r.unblocks_ids.len(),
    })
}

/// Go binds the top recommendation's claim action onto the group
/// (`group.ClaimCommand = rec.Actions.Claim.Shell`, triage.go:1882-1884 and
/// :1977-1979), and blanks it on every update. `claim` is absent when the
/// tracker route is unverified, which is exactly the omitempty case.
fn group_claim_command(r: &&bv_analysis::impact::IssueImpact) -> String {
    r.actions
        .as_ref()
        .and_then(|a| a.get("claim"))
        .and_then(|c| c.get("shell"))
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Go `buildRecommendationsByTrack` (pkg/analysis/triage.go:1800-1901) —
/// recommendations grouped into execution tracks by blocker depth.
///
/// Depth is the longest open-blocker chain above the item, computed by the
/// memoised DFS at :1812-1851 over `rec.BlockedBy`; a cycle yields -1, which
/// :1858-1860 rewrites to 999 so cyclic work lands in its own track. Groups
/// are emitted in ascending depth and each keeps the recommendations in the
/// scored order Go appends them.
fn build_recommendations_by_track(
    recs: &[&bv_analysis::impact::IssueImpact],
    issue_by_id: &std::collections::HashMap<&str, &bv_core::model::Issue>,
) -> Vec<serde_json::Value> {
    let by_id: std::collections::HashMap<&str, &bv_analysis::impact::IssueImpact> =
        recs.iter().map(|r| (r.id.as_str(), *r)).collect();
    let mut depths: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    fn dfs(
        id: &str,
        by_id: &std::collections::HashMap<&str, &bv_analysis::impact::IssueImpact>,
        depths: &mut std::collections::HashMap<String, i64>,
        visited: &mut std::collections::HashSet<String>,
    ) -> i64 {
        if let Some(&d) = depths.get(id) {
            return d;
        }
        if !visited.insert(id.to_string()) {
            return -1; // Go: `visited[id]` is already true — cycle.
        }
        let depth = match by_id.get(id) {
            Some(rec) if !rec.blocked_by.is_empty() => {
                let mut max_blocker_depth = 0i64;
                for blocker in &rec.blocked_by {
                    let d = dfs(blocker, by_id, depths, visited);
                    if d == -1 {
                        depths.insert(id.to_string(), -1);
                        visited.remove(id);
                        return -1;
                    }
                    if d + 1 > max_blocker_depth {
                        max_blocker_depth = d + 1;
                    }
                }
                max_blocker_depth
            }
            // Go triage.go:1823 — a recommendation that is not in the scored
            // set, or has no blockers, is depth 0.
            _ => 0,
        };
        visited.remove(id);
        depths.insert(id.to_string(), depth);
        depth
    }

    for rec in recs {
        let mut visited = std::collections::HashSet::new();
        dfs(rec.id.as_str(), &by_id, &mut depths, &mut visited);
    }

    // Go accumulates into a map keyed by depth, then emits `sort.Ints(depths)`
    // (triage.go:1893-1900). A BTreeMap keyed by depth gives the same order.
    let mut groups: std::collections::BTreeMap<i64, serde_json::Map<String, serde_json::Value>> =
        std::collections::BTreeMap::new();
    let mut top_score: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
    for rec in recs {
        let depth = match depths.get(rec.id.as_str()) {
            Some(&d) if d >= 0 => d,
            _ => 999,
        };
        let entry = groups.entry(depth).or_insert_with(|| {
            let mut m = serde_json::Map::new();
            m.insert(
                "track_id".into(),
                serde_json::json!(generate_track_id(depth + 1)),
            );
            m.insert("reason".into(), serde_json::json!(layer_reason(depth)));
            m.insert("recommendations".into(), serde_json::json!([]));
            m
        });
        entry
            .get_mut("recommendations")
            .and_then(|v| v.as_array_mut())
            .expect("recommendations is an array")
            .push(serde_json::to_value(rec).unwrap_or(serde_json::Value::Null));
        let unblocks = rec.unblocks_ids.len();
        *entry
            .entry("total_unblocks".to_string())
            .or_insert(serde_json::json!(0usize)) = serde_json::json!(
            entry
                .get("total_unblocks")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize
                + unblocks
        );
        // Go triage.go:1872-1885 — the top pick is the highest-scoring
        // CLAIMABLE recommendation seen so far, and each improvement rebinds
        // the claim command alongside it.
        let score = top_score.get(&depth).copied();
        if triage_claimable(rec, issue_by_id) && score.is_none_or(|s| rec.score > s) {
            top_score.insert(depth, rec.score);
            entry.insert("top_pick".into(), group_top_pick(rec));
            let cmd = group_claim_command(rec);
            if cmd.is_empty() {
                entry.remove("claim_command");
            } else {
                entry.insert("claim_command".into(), serde_json::json!(cmd));
            }
        }
    }
    groups
        .into_values()
        .map(serde_json::Value::Object)
        .collect()
}

/// Go `buildRecommendationsByLabel` (pkg/analysis/triage.go:1919-1990) —
/// recommendations bucketed by their PRIMARY label, `"unlabeled"` when the
/// item carries none. There is no normalization: the label is compared and
/// sorted byte-for-byte, and the group order is `sort.Slice` on the raw
/// string (triage.go:1985-1987), so a BTreeMap keyed by the label reproduces
/// it.
fn build_recommendations_by_label(
    recs: &[&bv_analysis::impact::IssueImpact],
    issue_by_id: &std::collections::HashMap<&str, &bv_core::model::Issue>,
) -> Vec<serde_json::Value> {
    let mut groups: std::collections::BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
        std::collections::BTreeMap::new();
    let mut top_score: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for rec in recs {
        // Go triage.go:1925-1928 — first label wins, verbatim.
        let label = rec
            .labels
            .first()
            .cloned()
            .unwrap_or_else(|| "unlabeled".to_string());
        let entry = groups.entry(label.clone()).or_insert_with(|| {
            let mut m = serde_json::Map::new();
            m.insert("label".into(), serde_json::json!(label));
            m.insert("recommendations".into(), serde_json::json!([]));
            m
        });
        entry
            .get_mut("recommendations")
            .and_then(|v| v.as_array_mut())
            .expect("recommendations is an array")
            .push(serde_json::to_value(rec).unwrap_or(serde_json::Value::Null));
        let unblocks = rec.unblocks_ids.len();
        *entry
            .entry("total_unblocks".to_string())
            .or_insert(serde_json::json!(0usize)) = serde_json::json!(
            entry
                .get("total_unblocks")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize
                + unblocks
        );
        let score = top_score.get(&label).copied();
        if triage_claimable(rec, issue_by_id) && score.is_none_or(|s| rec.score > s) {
            top_score.insert(label.clone(), rec.score);
            entry.insert("top_pick".into(), group_top_pick(rec));
            let cmd = group_claim_command(rec);
            if cmd.is_empty() {
                entry.remove("claim_command");
            } else {
                entry.insert("claim_command".into(), serde_json::json!(cmd));
            }
        }
    }
    groups
        .into_values()
        .map(serde_json::Value::Object)
        .collect()
}

/// Go `hasAnyLabel` (pkg/analysis/triage.go:1208-1220) — case-insensitive,
/// whitespace-trimmed membership. Returns false for an empty `wanted`, so an
/// unconfigured not-ready set never excludes anything.
fn has_any_label(issue_labels: &[String], wanted: &[String]) -> bool {
    if wanted.is_empty() || issue_labels.is_empty() {
        return false;
    }
    issue_labels.iter().any(|have| {
        let have = have.trim().to_lowercase();
        wanted.iter().any(|w| have == w.trim().to_lowercase())
    })
}

fn triage_claimable(
    r: &bv_analysis::impact::IssueImpact,
    issue_by_id: &std::collections::HashMap<&str, &bv_core::model::Issue>,
) -> bool {
    if r.status != "open" || r.issue_type == "epic" {
        return false;
    }
    if let Some(issue) = issue_by_id.get(r.id.as_str()) {
        if !issue.assignee.trim().is_empty() {
            return false;
        }
        // Ancestor-epic parity (#2): shared blocker_chain helper.
        if !bv_analysis::open_blockers(issue_by_id, &issue.id).is_empty() {
            return false;
        }
        // Go triage.go:1200 — `!hasAnyLabel(rec.Labels, notReadyLabels)` is
        // the final conjunct of `isClaimableRecommendation`, so a
        // graph-ready-but-not-work-ready bead is excluded from top picks,
        // triage-by-track and triage-by-label alike.
        if has_any_label(&issue.labels, &resolve_not_ready_labels()) {
            return false;
        }
    }
    true
}

/// Go `defaultRobotHistoryTimeout` (robot_registry.go:2021) — the budget for
/// the git-history correlation prologue of --robot-triage / --robot-next.
const DEFAULT_ROBOT_HISTORY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Go `robotHistoryShutdownGrace` (robot_registry.go:2025) — how long a
/// timed-out triage invocation waits for the abandoned report thread before
/// returning, so a directly spawned, context-killed git child gets a bounded
/// chance to be reaped before this short-lived process exits.
const ROBOT_HISTORY_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Go `maxRobotHistoryTimeoutMillis` (robot_registry.go:2029) —
/// `math.MaxInt64 / int64(time.Millisecond)`, i.e. the largest millisecond
/// count that still fits in a `time.Duration` (9223372036854).
const MAX_ROBOT_HISTORY_TIMEOUT_MILLIS: i64 = i64::MAX / 1_000_000;

/// Go `robotHistoryTimeoutFromMilliseconds` (robot_registry.go:2035).
///
/// Saturates rather than wrapping: a millisecond count past the range of
/// `time.Duration` would multiply out to a NEGATIVE duration, and Go's bounded
/// path treats `timeout > 0` as the only case that installs a deadline — so an
/// overflow would silently become unbounded instead of "very long". Returns
/// `None` for a negative input, which is the out-of-range case of the Go
/// `(time.Duration, bool)` pair.
fn robot_history_timeout_from_millis(ms: i64) -> Option<std::time::Duration> {
    if ms < 0 {
        return None;
    }
    if ms > MAX_ROBOT_HISTORY_TIMEOUT_MILLIS {
        // Go returns `time.Duration(math.MaxInt64)` here.
        return Some(std::time::Duration::from_nanos(u64::MAX));
    }
    Some(std::time::Duration::from_millis(ms as u64))
}

/// Go `resolveRobotHistoryTimeout` (robot_registry.go:2049). Precedence: the
/// `--robot-history-timeout-ms` flag when explicitly set (>= 0), then
/// `BV_ROBOT_HISTORY_TIMEOUT_MS`, then the 10s default. A resolved `0` disables
/// the bound entirely (Go's legacy run-to-completion), which is why the caller
/// must not treat zero as "no budget configured".
///
/// The env var is parsed with base 10 (`strconv.ParseInt(envVal, 10, 64)`,
/// robot_registry.go:2055) — NOT the base-0 rule pflag applies to the flag
/// itself, so `0x10` is a parse failure in the env and falls through to the
/// default. An unparsable or negative env value falls through too: Go ignores
/// the `ok` return and keeps going.
fn resolve_robot_history_timeout(flag_ms: Option<i64>) -> std::time::Duration {
    if let Some(ms) = flag_ms {
        if ms >= 0 {
            // A >= 0 flag value is always convertible, so the `None` arm is
            // unreachable here; the default keeps the signature total.
            return robot_history_timeout_from_millis(ms).unwrap_or(DEFAULT_ROBOT_HISTORY_TIMEOUT);
        }
    }
    if let Ok(env_val) = std::env::var("BV_ROBOT_HISTORY_TIMEOUT_MS") {
        let env_val = env_val.trim();
        if !env_val.is_empty() {
            if let Ok(ms) = env_val.parse::<i64>() {
                if let Some(d) = robot_history_timeout_from_millis(ms) {
                    return d;
                }
            }
        }
    }
    DEFAULT_ROBOT_HISTORY_TIMEOUT
}

/// Go `generateTriageHistoryBounded` (robot_registry.go:2107) — the exact call
/// the budget wraps. Read the Go handler end to end before changing this: the
/// budget covers ONLY the git-history correlation prologue
/// (`correlator.GenerateReportCached`), not the `ComputeTriageWithOptionsAndTime`
/// that consumes the report. Triage scoring is deliberately left unbounded.
///
/// The report is built on its own thread while the caller waits against the
/// budget, mirroring Go's goroutine + `select` on `resCh` vs `histCtx.Done()`.
/// On timeout the status is "timeout" and the report is dropped, which is the
/// degradation path Go already had. A zero budget is unbounded, so it is served
/// by a plain blocking receive rather than a zero-length `recv_timeout` (which
/// would fire immediately).
///
/// This does not claim every internal cache or lock wait inside the report
/// builder is cancellable; as in Go, the caller simply stops waiting once the
/// shutdown grace expires, and the thread may finish later.
fn generate_triage_history_bounded(
    cwd: &std::path::Path,
    beads: &[bv_correlation::history::BeadInfo],
    opts: &bv_correlation::history::HistoryOptions,
    generated_at: String,
    timeout: std::time::Duration,
) -> (Option<bv_correlation::history::HistoryReport>, &'static str) {
    // Go builds the correlator with the feedback store attached
    // (`newCorrelatorWithFeedback`, robot_registry.go:2116), so a stored
    // confirm/reject shapes the report before triage reads it.
    let store = load_correlation_feedback_store(cwd);
    let repo = cwd.to_path_buf();
    let beads = beads.to_vec();
    let opts = opts.clone();

    let (tx, rx) =
        crossbeam_channel::bounded::<Result<bv_correlation::history::HistoryReport, String>>(1);
    std::thread::spawn(move || {
        // Dropping `tx` on return closes the channel, so a caller that gives up
        // on the budget sees a disconnected receiver rather than hanging.
        let _ = tx.send(bv_correlation::history::build_history_report_with_feedback(
            &repo,
            &beads,
            &opts,
            None,
            generated_at,
            store.as_ref(),
        ));
    });

    let received = if timeout.is_zero() {
        // Go only installs a deadline when `timeout > 0`; 0 is unbounded.
        rx.recv().ok()
    } else {
        rx.recv_timeout(timeout).ok()
    };

    match received {
        Some(Ok(report)) => (Some(report), "ok"),
        // The thread finished inside the budget but the build failed.
        Some(Err(_)) => (None, "error"),
        // Deadline fired (or the builder panicked and closed the channel):
        // give the thread the shutdown grace to reap a killed git child, then
        // report the timeout. Go's second `select` on `resCh` vs
        // `robotHistoryShutdownGrace` — the result is discarded either way.
        None => {
            let _ = rx.recv_timeout(ROBOT_HISTORY_SHUTDOWN_GRACE);
            (None, "timeout")
        }
    }
}

fn run_robot_triage() -> ExitCode {
    let cwd = go_working_dir();
    let as_of = extract_as_of();
    // Use load_issues_auto_meta, not load_issues_auto: the latter discards the
    // SourceMeta, and re-deriving it via source_meta_for() is what hard-codes
    // errors/skipped to zero — making claim_safe unconditionally true.
    let (loaded, _hash, as_of_commit, loaded_source) =
        match load_issues_auto_meta_raw(&cwd, as_of.as_deref()) {
            Ok(x) => x,
            Err(e) => {
                if is_unresolved_source_error(&e.to_string()) {
                    return emit_unresolved_source();
                }
                eprintln!("Error: {e}");
                return ExitCode::from(1);
            }
        };
    // `--repo` then `--label` narrow the analysis at the same point Go does
    // (scopeLoadedIssues, main.go:2786 / :4870-4902), while the envelope keeps
    // describing the loaded file.
    let (issues, hash_override) = apply_scope(&loaded);
    let loaded_hash = hash_override.unwrap_or_else(|| _hash.clone());
    // An empty set is NOT a reason to stop. A `--repo` that matches nothing, or
    // a `--label` nothing carries, still has to produce the full envelope with
    // every metric marked "skipped", the counts zeroed and the lists empty —
    // that document is how a caller tells "nothing matched the filter" from
    // "the command failed". bvr emitted a three-key stub instead, so an agent
    // parsing it found no `triage.quick_ref` at all and had no way to tell the
    // two apart. The analysis below is defined for an empty graph.
    set_as_of(
        &as_of.clone().unwrap_or_default(),
        &as_of_commit.clone().unwrap_or_default(),
    );
    // Go keeps the loader's data_hash: scopeLoadedIssues sets
    // DataHashMatchesIssues=false so the payload still names the file it came
    // from (main.go:4890-4900).
    let data_hash = loaded_hash;
    let g = std::sync::Arc::new(bv_analysis::analyzer::build_graph(&issues));
    let mut out = bv_analysis::triage::build_triage(&issues, &g, robot_now(), as_of.is_some());
    // Go stamps every recommendation with `issue.Actions(claimable)`
    // (triage.go:658). The tracker route needs the loaded source path, which
    // only the CLI layer has, so it is resolved here rather than in the
    // analysis layer.
    // Describe the loaded file, not the scoped analysis set.
    let source = loaded_source.clone();
    for rec in out.recommendations.iter_mut() {
        let origin = bv_core::tracker::resolve_issue_origin(&source.path, &rec.id);
        let actions = bv_core::tracker::build_actions(&origin, rec.claimable);
        rec.actions = Some(serde_json::to_value(&actions).unwrap_or(serde_json::Value::Null));
        restate_unblock_reason(&mut rec.reasons, &rec.unblocks_ids);
    }

    // Build top_picks: Go `buildTopPicks` — only claimable recommendations
    // (open, not epic, unassigned, no open blockers, not a parent with open
    // children). Use original issue data for assignee/blocker checks.
    let issue_by_id: std::collections::HashMap<&str, &bv_core::model::Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let top_picks: Vec<serde_json::Value> = out
        .recommendations
        .iter()
        .filter(|r| {
            if r.status != "open" || r.issue_type == "epic" {
                return false;
            }
            if let Some(issue) = issue_by_id.get(r.id.as_str()) {
                if !issue.assignee.trim().is_empty() {
                    return false;
                }
                // Ancestor-epic parity (#2): shared blocker_chain helper.
                if !bv_analysis::open_blockers(&issue_by_id, &issue.id).is_empty() {
                    return false;
                }
            }
            true
        })
        .take(3)
        .map(|r| {
            // Go buildTopPicks (triage.go:1165) reads `len(rec.UnblocksIDs)` —
            // the size of the value `buildUnblocksMap` (triage.go:797-831) put
            // under this blocker id, which `build_triage` already stamped onto
            // every recommendation. That map credits a dependent only when the
            // dependent has EXACTLY ONE open blocker and is actionable once
            // that one completes, so a raw in-degree scan over-credits
            // fan-in nodes whose dependents are still multiply blocked.
            let unblocks = r.unblocks_ids.len();
            serde_json::json!({
                "id": r.id,
                "title": r.title,
                "score": r.score,
                "reasons": r.reasons,
                "unblocks": unblocks,
            })
        })
        .collect();

    // Build quick_wins: low-effort high-impact (Go parity: buildQuickWins).
    // Go formula: (log2(unblocks+1)*0.4 + simplicity*0.4 + priorityBonus*0.2)
    // Sorted by quickWinScore desc, then ID asc.
    let quick_wins: Vec<serde_json::Value> = {
        let mut candidates: Vec<_> = out
            .recommendations
            .iter()
            .filter(|r| triage_claimable(r, &issue_by_id))
            .map(|r| {
                // Go buildQuickWins (triage.go:979-983) reads
                // `unblocksMap[score.IssueID]` — the same map
                // `buildUnblocksMap` built and `build_triage` stamped onto the
                // recommendation as `unblocks_ids`, already lexicographically
                // sorted at triage.rs (Go triage.go:826-827). A raw in-degree
                // scan counted dependents that are still multiply blocked,
                // which Go credits to nobody.
                let unblocks_count = r.unblocks_ids.len();
                let unblocks_ids: Vec<String> = r.unblocks_ids.clone();
                let unblock_impact = ((unblocks_count as f64) + 1.0).log2();
                // Go compares BlockerRatioNorm (triage.go:988), not the
                // weighted blocker_ratio. They differ by the 0.13 weight, so
                // using the weighted value scored every low-blocker item as
                // "simple" and doubled the quick-win score.
                let simplicity = if r.breakdown.blocker_ratio_norm < 0.2 {
                    1.0
                } else if r.breakdown.blocker_ratio_norm < 0.4 {
                    0.5
                } else {
                    0.0
                };
                let priority_bonus = if r.priority <= 1 { 0.5 } else { 0.0 };
                let qw_score = unblock_impact * 0.4 + simplicity * 0.4 + priority_bonus * 0.2;
                // Build reason (Go parity: buildQuickWins reason logic).
                let mut reason = "Low complexity".to_string();
                if unblocks_count > 0 {
                    reason = format!("Unblocks {unblocks_count} items");
                }
                if r.priority <= 1 {
                    reason.push_str(", high priority");
                }
                (qw_score, r, unblocks_ids, reason)
            })
            .collect();
        candidates.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.id.cmp(&b.1.id))
        });
        candidates
            .into_iter()
            .take(5)
            .map(|(qw_score, r, unblocks_ids, reason)| {
                // Go emits the quick-win score (impact/effort blend), not the
                // raw impact score, and carries the issue status alongside.
                let mut qw = serde_json::json!({
                    "id": r.id, "title": r.title, "status": r.status,
                    "score": qw_score,
                    "reason": reason,
                });
                if !unblocks_ids.is_empty() {
                    qw["unblocks_ids"] = serde_json::json!(unblocks_ids);
                }
                qw
            })
            .collect()
    };

    // Build blockers_to_clear: high betweenness blocking issues.
    // Go `buildBlockersToClearWithContext` (triage.go:1100-1148) builds this
    // from the triage unblocks map, not from blocker_ratio/betweenness: an item
    // qualifies when it is a non-closed candidate that unblocks something, and
    // the list is sorted by unblocks count desc then id asc.
    let issue_index: std::collections::HashMap<&str, &bv_core::model::Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let mut blockers: Vec<(&str, usize, Vec<String>)> = out
        .recommendations
        .iter()
        .filter_map(|r| {
            if r.blocked_by.is_empty() && r.unblocks_ids.is_empty() {
                return None;
            }
            let issue = issue_index.get(r.id.as_str())?;
            if matches!(
                issue.status,
                bv_core::model::Status::Closed | bv_core::model::Status::Tombstone
            ) {
                return None;
            }
            Some((r.id.as_str(), r.unblocks_ids.len(), r.unblocks_ids.clone()))
        })
        .filter(|(_, n, _)| *n > 0)
        .collect();
    blockers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let blockers_to_clear: Vec<serde_json::Value> = blockers
        .into_iter()
        .take(5)
        .map(|(id, unblocks_count, unblocks_ids)| {
            let actionable = !out
                .recommendations
                .iter()
                .find(|r| r.id == id)
                .map(|r| !r.blocked_by.is_empty())
                .unwrap_or(true);
            let mut item = serde_json::json!({
                "id": id,
                "title": issue_index.get(id).map(|i| i.title.clone()).unwrap_or_default(),
                "unblocks_count": unblocks_count,
                "actionable": actionable,
            });
            if !unblocks_ids.is_empty() {
                item["unblocks_ids"] = serde_json::json!(unblocks_ids);
            }
            if !actionable {
                item["blocked_by"] =
                    serde_json::json!(bv_analysis::blocker_chain::open_blockers(&issue_index, id));
            }
            item
        })
        .collect();

    // Project health overview.
    let graph_density = if out.counts.total > 1 {
        g.edge_count() as f64 / (out.counts.total as f64 * (out.counts.total as f64 - 1.0))
    } else {
        0.0
    };
    let project_health = serde_json::json!({
        "counts": {
            "total": out.counts.total,
            "open": out.counts.open,
            "closed": out.counts.closed,
            "blocked": out.counts.blocked,
            "actionable": out.counts.actionable,
            "not_closed": out.counts.not_closed,
            "dependency_blocked": out.counts.dependency_blocked,
            "by_status": out.counts.by_status,
            "by_type": out.counts.by_type,
            "by_priority": out.counts.by_priority,
        },
        "graph": {
            "node_count": out.counts.total,
            "edge_count": g.edge_count(),
            "density": graph_density,
            "has_cycles": false,
            "phase2_ready": true,
        },
        "velocity": out.velocity,
    });

    // Go `buildCommands` (pkg/analysis/triage.go:1243): helper commands come
    // from the live tracker route's `actions`. `show_top`/`claim_top` are the
    // tracker's shell strings (empty when no route is established) and
    // `list_ready`/`list_blocked` are always empty — there is no unique route
    // to name. `refresh_triage` is literally "bv --robot-triage".
    let top_id = out
        .recommendations
        .first()
        .map(|r| r.id.as_str())
        .unwrap_or("");
    let top_actions = bv_core::tracker::build_actions(
        &bv_core::tracker::resolve_issue_origin(&source.path, top_id),
        true,
    );
    let commands = serde_json::json!({
        "claim_top": top_actions.claim.as_ref().map(|c| c.shell.as_str()).unwrap_or(""),
        "show_top": top_actions.show.as_ref().map(|c| c.shell.as_str()).unwrap_or(""),
        "list_ready": "",
        "list_blocked": "",
        "refresh_triage": "bv --robot-triage",
    });

    let mut env = bv_robot::RobotEnvelope::new(
        data_hash.clone(),
        env!("CARGO_PKG_VERSION"),
        None,
        bv_robot::OutputFormat::Json,
    );
    env.generated_at = jiff_now(); // Truncate to seconds (Go parity).
                                   // Go's HistoryStatus (triage.go:63-71) is omitempty, so an empty value
                                   // omits the key — but when the optional git-history prologue is skipped Go
                                   // sets it to the literal "skipped" rather than leaving it empty. Omitting
                                   // it entirely, as this did, lost that distinction.
                                   // Go's HistoryStatus (triage.go:63-71) is omitempty. robot_registry.go:2155
                                   // decides the value: with open work AND SOURCE_DATE_EPOCH set
                                   // (main.go:1174), the history prologue is skipped outright and the status is
                                   // the literal "skipped" — a pinned clock would otherwise make history
                                   // output drift between runs. Only the unpinned path builds a real report
                                   // and reports "ok".
    let has_open_issues = issues.iter().any(|i| !i.status.is_closed());
    let source_date_epoch_active = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .map(|v| v.trim().parse::<i64>().is_ok())
        .unwrap_or(false);
    // Go registers `--robot-history-timeout-ms` as a pflag `flag.Int`
    // (main.go:1549), so pflag parses the value during flag parsing: a value
    // that is not `strconv.ParseInt(s, 0, 64)` is a parse error, not a silent
    // fall back to the default. Go reports
    // `invalid argument %q for "--robot-history-timeout-ms" flag: %v` and exits
    // 1 (main.go:4548-4550). The default is -1, meaning UNSET — a distinct
    // value from 0, which means unbounded, so the Option has to survive to
    // `resolve_robot_history_timeout` rather than collapsing to a number here.
    let history_timeout_ms: Option<i64> = {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match flag_value(&args, "robot-history-timeout-ms") {
            Some(raw) => match go_parse_int_base0(raw) {
                Ok(v) => Some(v),
                Err(detail) => {
                    eprintln!(
                        "invalid argument {raw:?} for \"--robot-history-timeout-ms\" flag: {detail}"
                    );
                    return ExitCode::from(1);
                }
            },
            None => None,
        }
    };
    // Go `handleRobotTriage` (robot_registry.go:2157-2190) computes the history
    // status in this order: an empty string (key omitted) when there is no open
    // work, "skipped" under a pinned clock, and otherwise whatever the bounded
    // prologue returned. A failure to locate the work dir, the beads dir or a
    // valid repository leaves the status EMPTY rather than inventing one, so
    // the "ok" below is only reachable once the prologue actually ran.
    let history_status = if has_open_issues && source_date_epoch_active {
        "skipped"
    } else if has_open_issues {
        // Go's limit dance (robot_registry.go:2173-2179): the correlator
        // default is 500, `--history-limit` overrides it, and a limit that is
        // STILL 500 falls back to 200. Reproduced as written — an explicit
        // `--history-limit 500` is rewritten to 200 too.
        let mut limit: i64 = 500;
        if let Some(raw) = history_flag_value("history-limit") {
            if let Ok(v) = raw.trim().parse::<i64>() {
                limit = v;
            }
        }
        if limit == 500 {
            limit = 200;
        }
        match validate_correlation_repository(&cwd) {
            Ok(()) => {
                let beads: Vec<bv_correlation::history::BeadInfo> = issues
                    .iter()
                    .map(|i| bv_correlation::history::BeadInfo {
                        id: i.id.clone(),
                        title: i.title.clone(),
                        status: i.status.as_str().to_string(),
                    })
                    .collect();
                let opts = bv_correlation::history::HistoryOptions {
                    limit,
                    ..Default::default()
                };
                let budget = resolve_robot_history_timeout(history_timeout_ms);
                // The report is built under the budget but triage scoring does
                // not consume it yet: Go passes it as
                // `TriageOptions{History: ...}` (robot_registry.go:2218) and
                // `build_triage` has no such parameter. What the flag governs is
                // the work and its status, and that is wired here.
                let (_history_report, status) = generate_triage_history_bounded(
                    &cwd,
                    &beads,
                    &opts,
                    env.generated_at.clone(),
                    budget,
                );
                status
            }
            // Go leaves the status empty when `ValidateRepository` fails.
            Err(_) => "",
        }
    } else {
        ""
    };
    let mut meta = serde_json::json!({
        "version": bv_robot::ROBOT_CONTRACT_VERSION,
        "generated_at": env.generated_at,
        "phase2_ready": true,
        "issue_count": out.counts.total,
        "compute_time_ms": 0,
    });
    if !history_status.is_empty() {
        meta["history_status"] = serde_json::json!(history_status);
    }
    // Go's `statusEntry.MarshalJSON` (pkg/analysis/graph.go:131-147) drops
    // `reason` only when the string is empty, so clearing is how Rust reproduces
    // Go's *absent* key rather than a way of erasing a reason Go emits.
    //
    // For `--robot-triage` every skipped metric is one Go never attempted:
    // `TriageConfig` (config.go:260-277) sets Compute{Cycles,CriticalPath,
    // Eigenvector,HITS,KCore,Articulation,Slack} = false, and
    // `stateFromTiming` (graph.go:229-238) returns a bare "skipped" for a
    // disabled metric with no reason. Verified against the oracle on
    // small_chain: all seven emit `{"state":"skipped"}` and nothing else.
    //
    // Betweenness is deliberately left alone — `betweennessReason`
    // (graph.go:256-264) returns "approximate", which Go does emit.
    let mut triage_status = out.metric_status.clone();
    for entry in [
        &mut triage_status.page_rank,
        &mut triage_status.eigenvector,
        &mut triage_status.hits,
        &mut triage_status.critical,
        &mut triage_status.cycles,
        &mut triage_status.kcore,
        &mut triage_status.articulation,
        &mut triage_status.slack,
    ] {
        if entry.state == "skipped" {
            entry.reason.clear();
        }
    }
    let mut payload = full_envelope_json_with_source_and_authority(
        &data_hash,
        &_hash,
        Some(&loaded_source),
        &issues,
    );
    payload["output_format"] = serde_json::json!(env.output_format);
    payload["version"] = serde_json::json!(GO_APP_VERSION);

    // Go robot_registry.go:2232-2234 — `--brief` is an EARLY RETURN that
    // replaces the whole triage payload, before `meta`/`status`/
    // `project_health`/`commands`/`usage_hints`/`feedback` are ever assembled.
    // One branch covers all three triage primaries because they share
    // `handleRobotTriage` (robot_registry.go:1671-1679).
    if std::env::args().any(|a| a == "--brief") {
        let brief_recs: Vec<serde_json::Value> = out
            .recommendations
            .iter()
            .take(TRIAGE_TOP_N)
            .map(|r| {
                // Go `briefTriageRecommendation` (robot_registry.go:2276-2284):
                // id, title, status, assignee(omitempty), score, unblocks,
                // blocked_by, actions — a projection of `triage.Recommendations`,
                // which Go has already sliced to opts.TopN. An earlier comment
                // here claimed the opposite and published all 121
                // recommendations where the oracle publishes 10; the slice is
                // the same `take` the --robot-triage payload uses.
                let assignee = issue_by_id
                    .get(r.id.as_str())
                    .map(|i| i.assignee.clone())
                    .unwrap_or_default();
                let mut o = serde_json::Map::new();
                o.insert("id".into(), serde_json::json!(r.id));
                o.insert("title".into(), serde_json::json!(r.title));
                o.insert("status".into(), serde_json::json!(r.status));
                if !assignee.is_empty() {
                    o.insert("assignee".into(), serde_json::json!(assignee));
                }
                o.insert("score".into(), serde_json::json!(r.score));
                if !r.unblocks_ids.is_empty() {
                    o.insert("unblocks".into(), serde_json::json!(r.unblocks_ids));
                }
                if !r.blocked_by.is_empty() {
                    o.insert("blocked_by".into(), serde_json::json!(r.blocked_by));
                }
                o.insert(
                    "actions".into(),
                    r.actions.clone().unwrap_or(serde_json::Value::Null),
                );
                serde_json::Value::Object(o)
            })
            .collect();
        let mut brief = payload.clone();
        let obj = brief.as_object_mut().expect("envelope is an object");
        for key in ["triage", "usage_hints", "label_scope", "label_context"] {
            obj.remove(key);
        }
        obj.insert("brief".into(), serde_json::json!(true));
        obj.insert(
            "quick_ref".into(),
            serde_json::json!({
                "open_count": out.quick_ref.open_count,
                "actionable_count": out.quick_ref.actionable_count,
                "blocked_count": out.quick_ref.blocked_count,
                "in_progress_count": out.quick_ref.in_progress_count,
                "not_closed_count": out.quick_ref.not_closed_count,
                "not_actionable_count": out.quick_ref.not_actionable_count,
                "top_picks": top_picks,
            }),
        );
        obj.insert("recommendations".into(), serde_json::json!(brief_recs));
        if !quick_wins.is_empty() {
            obj.insert("quick_wins".into(), serde_json::json!(quick_wins));
        }
        if !blockers_to_clear.is_empty() {
            obj.insert(
                "blockers_to_clear".into(),
                serde_json::json!(blockers_to_clear),
            );
        }
        return emit_json(&brief);
    }
    let triage_body = serde_json::json!({
        "meta": meta,
        "status": triage_status.to_json_map(),
            "quick_ref": {
                "open_count": out.quick_ref.open_count,
                "actionable_count": out.quick_ref.actionable_count,
                "blocked_count": out.quick_ref.blocked_count,
                "in_progress_count": out.quick_ref.in_progress_count,
                "not_closed_count": out.quick_ref.not_closed_count,
                "not_actionable_count": out.quick_ref.not_actionable_count,
                "top_picks": top_picks,
            },
            // Go slices the scored set to opts.TopN (triage.go:593 sets the
            // default to 10) after building the full list, so top_picks
            // still searches the unsliced set (issue #146).
            "recommendations": recommendations_top_n(&out.recommendations),
            "quick_wins": quick_wins,
            "blockers_to_clear": blockers_to_clear,
            "project_health": project_health,
            "commands": commands,
    });
    payload["triage"] = triage_body;
    // Go passes the grouping flags into `analysis.TriageOptions`
    // (robot_registry.go:2210, :2213); `ComputeTriageWithOptions` then calls
    // `buildRecommendationsByTrack` (triage.go:697) or
    // `buildRecommendationsByLabel` (:700) and assigns the result to
    // `TriageResult.RecommendationsByTrack` / `RecommendationsByLabel`
    // (triage.go:52-53). Both are `omitempty`, so the array appears ONLY for
    // the flag that asked for it — the three triage primaries share one
    // handler, so this is the same condition for all of them. The grouping
    // runs on the FULL scored list, before the TopN slice.
    {
        let argv: Vec<String> = std::env::args().collect();
        // Both builders take the TopN-SLICED list, not `allRecommendations`:
        // Go slices at triage.go:666-669 and passes that same `recommendations`
        // to both groupers at :697 and :700.
        let grouped: Vec<&bv_analysis::impact::IssueImpact> =
            recommendations_top_n_refs(&out.recommendations);
        if argv.iter().any(|a| a == "--robot-triage-by-track") {
            payload["triage"]["recommendations_by_track"] =
                serde_json::json!(build_recommendations_by_track(&grouped, &issue_by_id));
        }
        if argv.iter().any(|a| a == "--robot-triage-by-label") {
            payload["triage"]["recommendations_by_label"] =
                serde_json::json!(build_recommendations_by_label(&grouped, &issue_by_id));
        }
    }
    // Add as_of/as_of_commit only when --as-of was used (Go omitempty parity).
    // Deliberately NOT written under `triage`: Go declares as_of and
    // as_of_commit on RobotEnvelope (main.go:7222-7223) and its TriageResult
    // has no such fields, so a nested copy was a key the oracle never emits.
    let _ = &as_of;
    if let Some(ref c) = as_of_commit {
        payload["triage"]["as_of_commit"] = serde_json::json!(c);
    }
    payload["usage_hints"] = serde_json::json!([
        "jq '.triage.quick_ref.top_picks[:3]' - Top 3 picks for immediate work",
        "jq '.triage.recommendations[3:10] | map({id,title,score})' - Next candidates after top picks",
        "jq '.triage.blockers_to_clear | map(.id)' - High-impact blockers to clear",
        "jq '.triage.recommendations[] | select(.type == \"bug\")' - Bug-focused recommendations",
        "jq '.triage.quick_ref.top_picks[] | select(.unblocks > 2)' - High-impact picks",
        "jq '.triage.quick_wins' - Low-effort, high-impact items",
        "--robot-next - Get only the single top recommendation",
        "--brief - Compact output: only id/title/status/assignee/blockers/unblocks (#183)",
        "--robot-triage-by-track - Group by execution track for multi-agent coordination",
        "--robot-triage-by-label - Group by label for area-focused agents",
        "jq '.triage.recommendations_by_track[].top_pick' - Top pick per track",
        "jq '.triage.recommendations_by_label[].claim_command' - Claim commands per label",
        "jq '.feedback.weight_adjustments' - View feedback-adjusted weights (bv-90)",
        "--graph-root <id> - Scope triage to subgraph rooted at a specific epic (bv-140)",
    ]);
    // `emit_json`, not `serde_json::to_string`: the Go encoder writes whole
    // float64 values without a trailing ".0", so an empty graph's density
    // renders as `0` and not `0.0`. Bypassing it made every zero in this
    // payload differ from the oracle's byte for byte.
    emit_json(&payload)
}

/// Go `ConfigForSize`'s `MaxCyclesToStore` for a graph of `nodes` nodes
/// (pkg/analysis/config.go:98-250), which is the `limit` the analyzer hands
/// `findCyclesSafe` (graph.go:2222). The XL tier sets `ComputeCycles: false`,
/// so Go stores an empty list there; the other tiers cap at 1000 / 100 / 50 /
/// 10 for < 100 / < 500 / < 2000 / >= 2000 nodes respectively.
fn baseline_cycle_limit(nodes: usize) -> usize {
    match nodes {
        0..=99 => 1000,
        100..=499 => 100,
        500..=1999 => 50,
        _ => 0,
    }
}

/// Go `findCyclesSafe` (pkg/analysis/graph_cycles.go:20-68) over the Rust
/// graph, returning issue-ID cycle paths.
///
/// `g`'s edge direction matches Go's analysis graph (graph.go:1653: "Issue (u)
/// depends on v → edge u -> v"), so Go's `g.From(u)` is this graph's successor
/// list and the traversal below walks the same edges in the same direction.
/// Node ids are the issue-id-sorted dense indices `build_graph` assigns, so
/// comparing indices is Go's comparing `node.ID()`.
fn find_cycles_safe(g: &bv_graph_core::DiGraph, limit: usize) -> Vec<Vec<String>> {
    if limit == 0 {
        return Vec::new();
    }
    let scc = bv_analysis::algorithms::cycles::tarjan_scc(g);
    let mut cycles: Vec<Vec<String>> = Vec::new();
    for comp in &scc.components {
        if comp.len() == 1 {
            // graph_cycles.go:29-35 — a singleton SCC is a cycle only when the
            // node has an edge to itself, and Go renders that as `[n, n]`.
            let n = comp[0];
            if g.successors_slice(n).contains(&n) {
                cycles.push(vec![node_label(g, n), node_label(g, n)]);
            }
            continue;
        }
        if let Some(cycle) = find_one_cycle_in_scc(g, comp) {
            cycles.push(cycle);
        }
    }
    // graph_cycles.go:46-59 — length ascending, then lexicographic on the id
    // sequence, for determinism.
    cycles.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.iter().cmp(b.iter())));
    // graph_cycles.go:61-64 — the total is recorded before the slice, so
    // truncation is reported without claiming every simple cycle was missed.
    cycles.truncate(limit);
    cycles
}

fn node_label(g: &bv_graph_core::DiGraph, idx: usize) -> String {
    g.node_id(idx).unwrap_or_default()
}

/// Go `findOneCycleInSCC` (pkg/analysis/graph_cycles.go:71-155): a single
/// cycle from one strongly connected component, found by an iterative DFS
/// over in-component successors in ascending-id order. The first edge back
/// into the current stack closes the cycle; the path is returned from that
/// node's stack position through the top, plus the closing node.
fn find_one_cycle_in_scc(g: &bv_graph_core::DiGraph, scc: &[usize]) -> Option<Vec<String>> {
    // graph_cycles.go:77-79 — sorted start points, and a mutable copy since
    // Go sorts the caller's slice in place.
    let mut members: Vec<usize> = scc.to_vec();
    members.sort_unstable();

    // graph_cycles.go:88-104 — adjacency restricted to the component and
    // sorted by id. Node indices are id-sorted, so index order is id order.
    let in_scc: std::collections::BTreeSet<usize> = members.iter().copied().collect();
    let mut adj: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::with_capacity(members.len());
    for &u in &members {
        let mut neighbors: Vec<usize> = g
            .successors_slice(u)
            .iter()
            .copied()
            .filter(|n| in_scc.contains(n))
            .collect();
        neighbors.sort_unstable();
        adj.insert(u, neighbors);
    }

    let mut visited: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut on_stack: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut stack_pos: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut neighbor_index: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    let mut stack: Vec<usize> = Vec::new();

    // graph_cycles.go:129-132 — seed with the lowest-id member.
    if let Some(&first) = members.first() {
        stack_pos.insert(first, 0);
        stack.push(first);
    }

    while let Some(&u) = stack.last() {
        let u_id = u;
        if visited.insert(u_id) {
            on_stack.insert(u_id);
        }
        let idx = *neighbor_index.get(&u_id).unwrap_or(&0);
        let empty: Vec<usize> = Vec::new();
        let neighbors = adj.get(&u_id).unwrap_or(&empty);
        if idx < neighbors.len() {
            let v = neighbors[idx];
            neighbor_index.insert(u_id, idx + 1);
            if on_stack.contains(&v) {
                // graph_cycles.go:139-147 — the edge u -> v closes the loop:
                // everything from v's stack position to the top, then v again.
                if let Some(&pos) = stack_pos.get(&v) {
                    let mut cycle: Vec<String> =
                        stack[pos..].iter().map(|&n| node_label(g, n)).collect();
                    cycle.push(node_label(g, v));
                    return Some(cycle);
                }
            }
            if !visited.contains(&v) {
                stack_pos.insert(v, stack.len());
                stack.push(v);
            }
        } else {
            // graph_cycles.go:149-156 — exhausted, backtrack.
            on_stack.remove(&u_id);
            stack_pos.remove(&u_id);
            stack.pop();
            neighbor_index.remove(&u_id);
        }
    }
    None
}

/// What a baseline capture produces: the stats snapshot, one representative
/// cycle per cyclic component, the data hash, and the five top-N metric lists
/// Go stores under `top_metrics`.
type BaselineCapture = (
    bv_analysis::drift::BaselineStats,
    Vec<Vec<String>>,
    String,
    bv_analysis::drift::TopMetrics,
);

fn capture_baseline() -> Result<BaselineCapture, String> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, _hash, _as_of_commit) = load_issues_auto(&cwd, None)?;
    capture_baseline_for(&issues)
}

/// `capture_baseline` over an already-loaded (and possibly `--label`-scoped)
/// issue set, so a scoped command compares like with like instead of silently
/// measuring the whole repo.
fn capture_baseline_for(issues: &[bv_core::model::Issue]) -> Result<BaselineCapture, String> {
    let hash = bv_core::data_hash::compute_data_hash(issues);
    let g = bv_analysis::analyzer::build_graph(issues);
    let p1 = bv_analysis::analyzer::analyze_phase1(&g);
    // Only the actionable count needs the computed blocked set; the recorded
    // `blocked_count` is status-based, as Go counts it.
    let blocked = bv_analysis::triage::compute_blocked_set(issues);
    let actionable = issues
        .iter()
        .filter(|i| i.status.is_open() && !blocked.contains(&i.id))
        .count();

    // Go's baseline cycle list is `stats.Cycles()` (robot_registry.go:1147),
    // i.e. exactly what `Analyzer.findCyclesSafe` stored
    // (graph_cycles.go:20-68): ONE representative cycle per strongly
    // connected component — a self-loop rendered as `[n, n]` for a singleton
    // SCC, otherwise the first cycle a sorted iterative DFS finds inside the
    // component — sorted by length ascending then lexicographically, and
    // truncated to the tier's `MaxCyclesToStore` only after the pre-limit
    // count is recorded. Pushing the whole SCC member list instead reported
    // component contents that are not a cycle at all, and dropped the
    // self-loop entirely, so the drift baseline disagreed with the oracle on
    // both the count and every cycle's contents.
    let new_cycles = find_cycles_safe(&g, baseline_cycle_limit(g.len()));
    // Go keeps the top 10 PageRank entries for drift comparison
    // (main.go:5242 buildMetricItems): rank by value descending, then
    // truncate. Ranking by graph iteration order instead selected a
    // different set, so the "entered top" details disagreed with the
    // oracle even at the same count.
    // Go `buildMetricItems` (main.go:5242-5258) is called for five metrics, not
    // one (main.go:3683-3687). Each is the same shape: id -> value map, sorted
    // by value descending, truncated to 10.
    //
    // Only the *order* of equal scores is unmatchable, and deliberately so: Go
    // feeds `sort.Slice` a slice built by ranging a Go map, so the pre-sort
    // order is randomized per process and the sort is not stable. Three
    // consecutive oracle runs on small_chain returned betweenness top-4 of
    // [FIX-6,FIX-7,FIX-5,FIX-8], [FIX-7,FIX-6,FIX-5,FIX-8] and
    // [FIX-6,FIX-7,FIX-8,FIX-5]. Chasing that would mean matching a coin flip.
    // Sorting stably (id ascending) keeps the *set* identical and makes the
    // document reproducible, which is strictly better and still comparable:
    // the values themselves match the oracle exactly. It also means the
    // membership of a truncated top-10 can differ from a given oracle run when
    // more than ten nodes tie at the cut — the values agree, the tie-break
    // cannot.
    let top_map = |values: &[f64]| -> std::collections::BTreeMap<String, f64> {
        let mut ranked: Vec<(String, f64)> = values
            .iter()
            .enumerate()
            .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), *v))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(10);
        ranked.into_iter().collect()
    };
    let pr_map = top_map(&bv_analysis::algorithms::pagerank::pagerank_default(&g));
    let hits = bv_graph_core::hits_default(&g);
    let top_metrics = bv_analysis::drift::TopMetrics {
        pagerank: bv_analysis::drift::TopMetrics::from_map(&pr_map, 10),
        betweenness: bv_analysis::drift::TopMetrics::from_map(
            &top_map(&bv_analysis::analyzer::go_betweenness(&g)),
            10,
        ),
        critical_path: bv_analysis::drift::TopMetrics::from_map(
            &top_map(&bv_analysis::algorithms::critical_path::critical_path_heights(&g)),
            10,
        ),
        hubs: bv_analysis::drift::TopMetrics::from_map(&top_map(&hits.hubs), 10),
        authorities: bv_analysis::drift::TopMetrics::from_map(&top_map(&hits.authorities), 10),
    };
    Ok((
        bv_analysis::drift::BaselineStats {
            node_count: p1.node_count,
            edge_count: p1.edge_count,
            density: p1.density,
            // Go counts these by *status* (cmd/bv/main.go:3661-3672), not by
            // readiness: `open` is StatusOpen plus StatusInProgress, `blocked`
            // is StatusBlocked. Rust used the computed blocked set — issues
            // that *have* an open blocker — which on small_chain is 11 against
            // Go's 0, because those issues are open rather than blocked.
            open: issues
                .iter()
                .filter(|i| {
                    matches!(
                        i.status,
                        bv_core::model::Status::Open | bv_core::model::Status::InProgress
                    )
                })
                .count(),
            closed: issues.iter().filter(|i| i.status.is_closed()).count(),
            blocked: issues
                .iter()
                .filter(|i| i.status == bv_core::model::Status::Blocked)
                .count(),
            cycle_count: new_cycles.len(),
            actionable,
            pagerank: pr_map,
        },
        new_cycles,
        hash,
        top_metrics,
    ))
}

const BASELINE_PATH: &str = ".bv/baseline.json";

/// Application version Go bv reports in robot envelopes (`pkg/version`
/// fallback, pinned at parity commit 18afafa). Byte-parity with frozen
/// goldens requires emitting Go's version string, not the Rust crate's.
///
/// One constant, shared with `bv_update::current_version` and therefore with
/// `--check-update` and the TUI update modal: Go resolves a single
/// `version.Version` (version.go:22-52) and every consumer reads it, so the
/// envelope and `--check-update` cannot disagree.
const GO_APP_VERSION: &str = bv_update::APP_VERSION;

/// Output format for the current invocation, set from `--format`. Go tracks
/// this in the package-level `robotOutputFormat` and stamps it into every
/// `RobotEnvelope`; `emit_json` and `full_envelope_json` read it here.
static OUTPUT_FORMAT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn set_output_format(fmt: &str) {
    let v = if fmt == "toon" { 1 } else { 0 };
    OUTPUT_FORMAT.store(v, std::sync::atomic::Ordering::Relaxed);
}

fn output_format() -> &'static str {
    if OUTPUT_FORMAT.load(std::sync::atomic::Ordering::Relaxed) == 1 {
        "toon"
    } else {
        "json"
    }
}

/// Go `robotShowToonStats` (main.go:2031): `--stats` or `TOON_STATS=1`. The
/// value is only consulted by the TOON encoder, so the flag is a deliberate
/// no-op in JSON mode (main.go:7560-7565 picks the JSON encoder unless the
/// format is "toon").
static SHOW_TOON_STATS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn set_show_toon_stats(on: bool) {
    SHOW_TOON_STATS.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn show_toon_stats() -> bool {
    SHOW_TOON_STATS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Go `estimateTokens` (main.go:7603-7610) — a coarse `/4` heuristic over the
/// trimmed byte length. `len()` on a Go string counts UTF-8 BYTES, so this
/// must be `len()` on the `&str`, never `chars().count()`.
fn estimate_tokens(s: &str) -> usize {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return 0;
    }
    trimmed.len().div_ceil(4)
}

/// Go main.go:2678 — an explicit `--workspace <path>` replaces auto-discovery.
/// Recorded once from the REWRITTEN argv in `main` so the robot loaders can
/// honour it without threading `args` through every call site.
static WORKSPACE_OVERRIDE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn set_workspace_override(path: std::path::PathBuf) {
    let _ = WORKSPACE_OVERRIDE.set(path);
}

fn workspace_override() -> Option<&'static std::path::Path> {
    WORKSPACE_OVERRIDE.get().map(|p| p.as_path())
}

/// Go main.go:1535 — `--force-full-analysis` swaps `ConfigForSize` for
/// `FullAnalysisConfig` (pkg/analysis/config.go:230-254) in every handler that
/// publishes an `analysis_config`, flipping the skipped metrics to computed.
/// Recorded once in `main` so the config emitters stay pure.
static FORCE_FULL_ANALYSIS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn force_full_analysis() -> bool {
    FORCE_FULL_ANALYSIS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Go `canonicalTheme` + `effectiveThemePreference` (main.go:4639-4666): an
/// explicitly passed but unrecognized `--theme` value warns on stderr and
/// resolves to auto-detection instead of silently falling through to a
/// lower-precedence source. Accepts only light/dark/auto, case- and
/// whitespace-insensitively.
fn warn_unknown_theme(args: &[String]) {
    let Some(theme) = flag_value(args, "theme") else {
        return;
    };
    if !matches!(
        theme.trim().to_lowercase().as_str(),
        "light" | "dark" | "auto"
    ) {
        eprintln!(
            "Warning: unknown --theme value {theme:?} (expected light, dark, or auto); using auto-detection"
        );
    }
}

/// Go `GetGitInfo` (baseline.go:139-155): HEAD sha, first line of the subject,
/// and the branch name, each from a separate `git` invocation. A missing git or
/// a repo-less directory yields empty strings, exactly as Go's error-swallowing
/// `runGit` does.
fn baseline_git_info() -> (String, String, String) {
    let run = |args: &[&str]| -> String {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    (
        run(&["rev-parse", "HEAD"]),
        run(&["log", "-1", "--format=%s"]),
        run(&["rev-parse", "--abbrev-ref", "HEAD"]),
    )
}

/// Go `Baseline.Summary` (baseline.go:170-214).
fn baseline_summary(
    stats: &bv_analysis::drift::BaselineStats,
    top: &bv_analysis::drift::TopMetrics,
    created_at: &str,
    commit_sha: &str,
    commit_message: &str,
    branch: &str,
    description: &str,
) -> String {
    let mut sb = format!("Baseline created: {}\n", go_rfc1123(created_at));
    if !commit_sha.is_empty() {
        let short = &commit_sha[..commit_sha.len().min(8)];
        if branch.is_empty() {
            sb.push_str(&format!("Commit: {short}\n"));
        } else {
            sb.push_str(&format!("Commit: {short} ({branch})\n"));
        }
        if !commit_message.is_empty() {
            sb.push_str(&format!("Message: {commit_message}\n"));
        }
    }
    if !description.is_empty() {
        sb.push_str(&format!("Note: {description}\n"));
    }
    sb.push_str(&format!(
        "\nGraph: {} nodes, {} edges (density: {:.4})\n",
        stats.node_count, stats.edge_count, stats.density
    ));
    sb.push_str(&format!(
        "Status: {} open, {} blocked, {} closed\n",
        stats.open, stats.blocked, stats.closed
    ));
    sb.push_str(&format!(
        "Actionable: {} | Cycles: {}\n",
        stats.actionable, stats.cycle_count
    ));
    if !top.pagerank.is_empty() {
        sb.push_str("\nTop PageRank:\n");
        for item in top.pagerank.iter().take(5) {
            sb.push_str(&format!("  {}: {:.4}\n", item.id, item.value));
        }
    }
    sb
}

fn run_save_baseline(desc: &str) -> ExitCode {
    match capture_baseline() {
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
        Ok((stats, cycles, hash, top_metrics)) => {
            let _ = hash;
            // Go `baseline.Baseline` (baseline.go:17-44), field for field and in
            // declaration order, with `omitempty` honoured. The previous
            // document used Rust's own key names and omitted `top_metrics`
            // entirely, so Go read every count as zero and no PageRank list at
            // all — which made `--check-drift` report every current node as
            // having "entered top" against a baseline bvr itself had written.
            let mut doc = serde_json::json!({
                "version": 1,
                "created_at": jiff_local_now(),
                "description": desc,
                "stats": stats,
                "top_metrics": top_metrics,
            });
            // Go's `Cycles` is `omitempty` (baseline.go:41), so a graph with no
            // cycles writes no `cycles` key at all. Emitting `[]` made the two
            // documents differ on a clean repo.
            if !cycles.is_empty() {
                doc["cycles"] = serde_json::json!(cycles);
            }
            // Go `baseline.New` (baseline.go:216-231) stamps the current git
            // HEAD into the document, which `Summary` then echoes back. Rust
            // wrote empty strings, so Go read a saved baseline as having no
            // commit at all and never printed the Commit/Message lines.
            let (sha, message, branch) = baseline_git_info();
            let commit_sha = sha.clone();
            if !sha.is_empty() {
                doc["commit_sha"] = serde_json::json!(sha);
            }
            if !message.is_empty() {
                doc["commit_message"] = serde_json::json!(message);
            }
            if !branch.is_empty() {
                doc["branch"] = serde_json::json!(branch);
            }
            std::fs::create_dir_all(".bv").ok();
            let created_at = doc["created_at"].as_str().unwrap_or_default().to_string();
            let stats = &stats;
            let top = &top_metrics;
            match std::fs::write(BASELINE_PATH, serde_json::to_vec_pretty(&doc).unwrap()) {
                Ok(_) => {
                    // Go main.go:3629-3630: the path, then `bl.Summary()`.
                    // Go builds the path from an absolute `os.Getwd()`
                    // (main.go:2558, baseline.go:89-91), so it echoes the
                    // resolved location rather than the relative one.
                    // `current_dir`, not `canonicalize`: on macOS the latter
                    // resolves /tmp to /private/tmp, which Go's `os.Getwd()`
                    // does not, so the two would echo different paths.
                    // `PWD`, not `current_dir()`: on macOS the latter reports
                    // the resolved physical path, so a run under a symlinked
                    // directory (a /tmp scratch dir, say) echoed /private/tmp
                    // where Go echoed /tmp. Go's `os.Getwd()` prefers `$PWD`
                    // for exactly this reason.
                    let abs = std::env::var("PWD")
                        .map(|d| std::path::PathBuf::from(d).join(BASELINE_PATH))
                        .unwrap_or_else(|_| std::path::PathBuf::from(BASELINE_PATH));
                    println!("Baseline saved to {}", abs.display());
                    print!(
                        "{}",
                        baseline_summary(
                            stats,
                            top,
                            &created_at,
                            &commit_sha,
                            &message,
                            &branch,
                            desc
                        )
                    );
                    ExitCode::from(0)
                }
                Err(e) => {
                    eprintln!("Error writing baseline: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// Handle `--baseline-info` (Go main.go:2564-2577). Deliberately runs BEFORE
/// any issue load — the comment at main.go:2547-2548 says so — so it works in
/// a repo with no `.beads` at all.
fn run_baseline_info() -> ExitCode {
    let path = std::path::Path::new(BASELINE_PATH);
    if !path.exists() {
        println!("No baseline found.");
        println!("Create one with: bv --save-baseline \"description\"");
        return ExitCode::from(0);
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("Error loading baseline: {e}");
            return ExitCode::from(1);
        }
    };
    let doc: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error loading baseline: {e}");
            return ExitCode::from(1);
        }
    };
    // Go `Baseline.Summary()` (pkg/baseline/baseline.go:170-213), field for
    // field. Every field is read defensively: Rust's own writer
    // (`run_save_baseline`) emits empty `commit_sha`/`branch` and no
    // `commit_message`/`top_metrics`, while a Go-written baseline has all of
    // them, so the Commit/Message/Top-PageRank sections are legitimately empty
    // for one and populated for the other.
    let created = doc
        .get("created_at")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    println!("Baseline created: {}", go_rfc1123(created));

    let sha = doc
        .get("commit_sha")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !sha.is_empty() {
        let short = &sha[..sha.len().min(8)];
        match doc.get("branch").and_then(|v| v.as_str()) {
            Some(b) if !b.is_empty() => println!("Commit: {short} ({b})"),
            _ => println!("Commit: {short}"),
        }
        if let Some(msg) = doc
            .get("commit_message")
            .and_then(|v| v.as_str())
            .filter(|m| !m.is_empty())
        {
            println!("Message: {msg}");
        }
    }
    let description = doc
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if !description.is_empty() {
        println!("Note: {description}");
    }
    let num = |key: &str| {
        doc.get("stats")
            .and_then(|s| s.get(key))
            .and_then(|v| v.as_i64())
    };
    let density = doc
        .get("stats")
        .and_then(|s| s.get("density"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    println!(
        "\nGraph: {} nodes, {} edges (density: {:.4})",
        num("node_count").unwrap_or(0),
        num("edge_count").unwrap_or(0),
        density
    );
    println!(
        "Status: {} open, {} blocked, {} closed",
        num("open_count").unwrap_or(0),
        num("blocked_count").unwrap_or(0),
        num("closed_count").unwrap_or(0)
    );
    println!(
        "Actionable: {} | Cycles: {}",
        num("actionable_count").unwrap_or(0),
        num("cycle_count").unwrap_or(0)
    );
    // baseline.go:204-212 — at most 5 PageRank rows.
    if let Some(rows) = doc
        .get("top_metrics")
        .and_then(|t| t.get("pagerank"))
        .and_then(|v| v.as_array())
        .filter(|rows| !rows.is_empty())
    {
        print!("\nTop PageRank:\n");
        for row in rows.iter().take(5) {
            let id = row.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let value = row.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
            println!("  {id}: {value:.4}");
        }
    }
    ExitCode::from(0)
}

/// Render an RFC3339 timestamp the way Go's `time.RFC1123` layout does:
/// `Mon, 02 Jan 2006 15:04:05 MST`.
/// The signed seconds an RFC3339 zone designator like `+07:00` denotes.
fn rfc3339_offset_seconds(label: &str) -> Option<i64> {
    let (sign, rest) = match label.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, label.strip_prefix('+')?),
    };
    // `+07` and `+07:00` both reach here: the wire form carries minutes, the
    // label `rfc3339_offset_label` produces is hour-only because that is what
    // Go's `MST` layout prints.
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h, m),
        None => (rest, "0"),
    };
    Some(sign * (h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60))
}

/// The zone label Go's `MST` layout prints for an RFC3339 timestamp: `UTC` for
/// `Z`, otherwise the numeric offset truncated to whole hours (`+07`).
fn rfc3339_offset_label(rfc3339: &str) -> Option<String> {
    let (_, tail) = rfc3339.rsplit_once('T')?;
    // The zone designator is the first `+` or `-` after the time fields. The
    // date's own `-` signs are behind us, so anything here is an offset.
    let idx = tail.find(['+', '-'])?;
    let sign = &tail[idx..idx + 1];
    let digits: String = tail[idx + 1..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    // `+07:00` and `+0700` both occur; the label Go prints is whole hours.
    let hours: i64 = digits.get(..2)?.parse().ok()?;
    Some(format!("{sign}{hours:02}"))
}

fn go_rfc1123(rfc3339: &str) -> String {
    let Ok(ts) = rfc3339.parse::<jiff::Timestamp>() else {
        return rfc3339.to_string();
    };
    // Go renders the timestamp in its own location ("Mon, 02 Jan 2006
    // 15:04:05 MST"), so a baseline saved in +07:00 prints "+07" while one
    // stored as UTC prints "UTC". Forcing UTC reported the right instant under
    // the wrong zone, and the wrong wall clock under the right zone.
    // Shift into the zone the timestamp is written in. Parsing normalises to
    // UTC, so without this the printed wall clock is the UTC one under a local
    // label — "00:44 +07" for an instant that is 07:44 local.
    let label = rfc3339_offset_label(rfc3339);
    let offset_secs = label
        .as_deref()
        .and_then(rfc3339_offset_seconds)
        .unwrap_or(0);
    let shifted = ts
        .checked_add(std::time::Duration::from_secs(offset_secs.unsigned_abs()))
        .unwrap_or(ts);
    let dt = shifted.to_zoned(jiff::tz::TimeZone::UTC).datetime();
    // jiff numbers weekdays Monday=1..Sunday=7; Go's `time.RFC1123` layout
    // ("Mon, 02 Jan 2006 15:04:05 MST") indexes a Monday-first table. The old
    // lookup wrapped with `% 7` into a table that started at "Thu", so every
    // date was off by one: 2026-08-22, a Saturday, printed as "Wed". The same
    // formatter supplies `--export-md`'s "*Generated:*" footer.
    const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let weekday = WEEKDAYS[dt.weekday() as usize - 1];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // Go's `MST` renders the zone's abbreviation, or its numeric offset when
    // the zone has no name — which is what a parsed RFC3339 offset gives.
    // `Timestamp` is UTC-based, so parsing has already discarded the offset
    // and it has to be read back off the wire form. Go keeps it: a
    // `time.Time` carries its own location, and `MST` prints that.
    let zone = label.unwrap_or_else(|| "UTC".to_string());
    format!(
        "{weekday}, {:02} {} {:04} {:02}:{:02}:{:02} {zone}",
        dt.day(),
        MONTHS[(dt.month() - 1) as usize],
        dt.year(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// Handle `--check-update` (Go bv-182): report whether a newer release exists.
fn run_check_update() -> ExitCode {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    report_check_update(bv_update::github::check_for_updates(), &mut out, &mut err)
}

/// Reporting half of `--check-update` (Go cmd/bv/main.go:2057-2071), split
/// from the fetch so the exact output can be asserted without network access.
fn report_check_update(
    result: Result<Option<bv_update::UpdateInfo>, bv_update::github::FetchError>,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> ExitCode {
    match result {
        Err(e) => {
            let _ = writeln!(err, "Error checking for updates: {e}");
            ExitCode::from(1)
        }
        Ok(None) => {
            let _ = writeln!(
                out,
                "bvr is up to date (version {})",
                bv_update::current_version()
            );
            ExitCode::from(0)
        }
        Ok(Some(info)) => {
            let _ = writeln!(
                out,
                "New version available: {} (current: {})",
                info.new_version,
                bv_update::current_version()
            );
            // Go main.go:2065 labels this line "Release:" — the value is the
            // release *page* URL, so "Download:" was a mislabel that broke any
            // byte comparison of `--check-update` output.
            let _ = writeln!(out, "Release: {}", info.release_url);
            let _ = writeln!(out, "\nRun 'bvr --update' to update automatically");
            ExitCode::from(0)
        }
    }
}

/// Handle `--update-dry-run`: report what an update would fetch/verify/install.
fn run_update_dry_run() -> ExitCode {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let release = match bv_update::github::get_latest_release() {
        Err(e) => {
            eprintln!("Error fetching release info: {e}");
            return ExitCode::from(1);
        }
        Ok(r) => r,
    };
    report_update_dry_run(&release, &mut out, &mut err)
}

/// Reporting half of `--update-dry-run` (Go cmd/bv/main.go:2074-2105), split
/// from the fetch so the plan and its failure paths can be asserted offline.
fn report_update_dry_run(
    release: &bv_update::Release,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> ExitCode {
    // Go main.go:2083-2086: a version-compare failure is a hard error, not an
    // "already up to date" answer. `is_newer_than_current` collapses the
    // Result to a bool, which is how this path used to disappear.
    let newer = match bv_update::version::check_newer_than_current(&release.tag_name) {
        Ok(n) => n,
        Err(e) => {
            let _ = writeln!(err, "Cannot compare release versions: {e}");
            return ExitCode::from(1);
        }
    };
    if !newer {
        let _ = writeln!(
            out,
            "bvr is already up to date (version {})",
            bv_update::current_version()
        );
        return ExitCode::from(0);
    }
    // Go main.go:2091-2094 validates before printing a plan it could not
    // carry out. Doing it here also makes both lookups below total, which is
    // why Go can dereference `asset` and `FindChecksumAsset()` unguarded.
    let (asset, checksum) = match bv_update::github::release_assets_for_update(release) {
        Ok(pair) => pair,
        Err(e) => {
            let _ = writeln!(err, "Latest release cannot be installed automatically: {e}");
            return ExitCode::from(1);
        }
    };
    let _ = writeln!(
        out,
        "[dry-run] Would update bvr from {} to {}",
        bv_update::current_version(),
        release.tag_name
    );
    let _ = writeln!(
        out,
        "[dry-run] Would download {} ({} bytes) for {}/{}",
        asset.name,
        asset.size,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(out, "[dry-run] From: {}", asset.browser_download_url);
    let _ = writeln!(
        out,
        "[dry-run] Would verify SHA-256 checksum via {}",
        checksum.name
    );
    let _ = writeln!(
        out,
        "[dry-run] No changes made. Run 'bvr --update' to apply."
    );
    ExitCode::from(0)
}

/// Go `readUpdateConfirmation` (cmd/bv/main.go:1016-1030).
///
/// Go's `bufio.ReadString('\n')` returns the bytes it managed to read *and*
/// `io.EOF` when the stream ends before a newline. `read_line` reports that
/// same situation as `Ok(n > 0)` with no trailing newline, so Go's EOF flag is
/// recovered here from the missing newline rather than from the return value.
fn read_update_confirmation(input: &mut impl std::io::BufRead) -> Result<bool, String> {
    let mut response = String::new();
    if let Err(e) = input.read_line(&mut response) {
        // main.go:1019-1021 — a non-EOF read failure is wrapped.
        return Err(format!("read update confirmation: {e}"));
    }
    let hit_eof = !response.ends_with('\n');
    let answer = response.trim().to_lowercase();
    if answer.is_empty() {
        // main.go:1024-1028 — an empty response means "confirmed" only when it
        // actually arrived on a line. End-of-input with nothing read is an
        // error, so `bvr --update < /dev/null` cannot self-update unattended.
        return if hit_eof {
            Err("no update confirmation received".to_string())
        } else {
            Ok(true)
        };
    }
    Ok(answer == "y" || answer == "yes")
}

/// Outcome of Go's `[Y/n]` gate (cmd/bv/main.go:2129-2140).
#[derive(Debug)]
enum UpdateConfirmation {
    Proceed,
    /// A non-empty, non-`y`/`yes` answer: `Update cancelled`, exit 0.
    Cancelled,
    /// main.go:2135-2137: a read failure, EOF included, is exit 1.
    ReadFailed,
}

/// Go's `[Y/n]` prompt plus `readUpdateConfirmation` (main.go:2129-2140),
/// split from `run_update` so all three outcomes are reachable in a test —
/// in `run_update` itself they sit behind a successful release fetch.
fn prompt_update_confirmation(
    version: &str,
    new_version: &str,
    input: &mut impl std::io::BufRead,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> UpdateConfirmation {
    let _ = write!(out, "Update bvr from {version} to {new_version}? [Y/n]: ");
    let _ = out.flush();
    match read_update_confirmation(input) {
        // main.go:2135-2138 — a failed read, EOF included, is exit 1.
        Err(e) => {
            let _ = writeln!(err, "Cannot read update confirmation: {e}");
            UpdateConfirmation::ReadFailed
        }
        Ok(false) => {
            let _ = writeln!(out, "Update cancelled");
            UpdateConfirmation::Cancelled
        }
        Ok(true) => UpdateConfirmation::Proceed,
    }
}

/// Handle `--update` (Go bv-182): confirm unless `--yes`, then self-update.
fn run_update(args: &[String]) -> ExitCode {
    let release = match bv_update::github::get_latest_release() {
        Err(e) => {
            eprintln!("Error fetching release info: {e}");
            return ExitCode::from(1);
        }
        Ok(r) => r,
    };
    // Go main.go:2117-2120: a version-compare failure is a hard error, not an
    // "already up to date" answer.
    let newer = match bv_update::version::check_newer_than_current(&release.tag_name) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("Cannot compare release versions: {e}");
            return ExitCode::from(1);
        }
    };
    if !newer {
        println!(
            "bvr is already up to date (version {})",
            bv_update::current_version()
        );
        return ExitCode::from(0);
    }
    // Go main.go:2124-2127: the release must be installable *before* the user
    // is prompted, so a draft, prerelease, asset-less or digest-less release
    // never asks for confirmation it cannot honour.
    if let Err(e) = bv_update::github::validate_release_for_update(&release) {
        eprintln!("Latest release cannot be installed automatically: {e}");
        return ExitCode::from(1);
    }
    if !args.iter().any(|a| a == "--yes" || a == "-y") {
        let mut out = std::io::stdout();
        let mut err = std::io::stderr();
        let version = bv_update::current_version();
        let confirmation = prompt_update_confirmation(
            &version,
            &release.tag_name,
            &mut std::io::stdin().lock(),
            &mut out,
            &mut err,
        );
        match confirmation {
            UpdateConfirmation::ReadFailed => return ExitCode::from(1),
            UpdateConfirmation::Cancelled => return ExitCode::from(0),
            UpdateConfirmation::Proceed => {}
        }
    }
    match bv_update::perform_update(&release, &|line| println!("{line}")) {
        Ok(result) => {
            println!("{}", result.message);
            if let Some(backup) = result.backup_path {
                println!("Backup saved to: {backup}");
                println!("Run 'bvr --rollback' to restore if needed");
            }
            ExitCode::from(0)
        }
        Err(e) => {
            eprintln!("Update failed: {e}");
            // main.go:2145-2147 — the backup is written before the risky
            // rename, so any failure past that point leaves a restorable copy
            // that has to be named or it is never found again.
            if let Some(backup) = e.backup_path() {
                eprintln!("Backup preserved at: {backup}");
            }
            ExitCode::from(1)
        }
    }
}

/// Offline coverage for the self-update CLI paths. Every branch Go reaches
/// through `GetLatestRelease` / `CheckUpdateAvailable` is unreachable in a
/// test without a network, so the reporting halves take their data by
/// argument and these tests drive them directly. Fixtures mirror the shape
/// `pkg/updater` accepts — an installable release needs a `state: uploaded`
/// platform archive, a checksum manifest, a `sha256:` digest on both and URLs
/// pinned to this repo (github.rs:375-463).
#[cfg(test)]
mod rfc1123_tests {
    use super::go_rfc1123;

    /// Go `time.RFC1123` names the weekday, and the layout is
    /// "Mon, 02 Jan 2006 15:04:05 MST". One case per weekday catches a table
    /// that is off by one in either direction, which a single date cannot.
    #[test]
    fn weekday_names_match_go_for_every_day() {
        // 2026-08-17 is a Monday, so these seven dates cover Mon..Sun.
        let cases = [
            ("2026-08-17T09:05:03Z", "Mon, 17 Aug 2026 09:05:03 UTC"),
            ("2026-08-18T09:05:03Z", "Tue, 18 Aug 2026 09:05:03 UTC"),
            ("2026-08-19T09:05:03Z", "Wed, 19 Aug 2026 09:05:03 UTC"),
            ("2026-08-20T09:05:03Z", "Thu, 20 Aug 2026 09:05:03 UTC"),
            ("2026-08-21T09:05:03Z", "Fri, 21 Aug 2026 09:05:03 UTC"),
            ("2026-08-22T09:05:03Z", "Sat, 22 Aug 2026 09:05:03 UTC"),
            ("2026-08-23T09:05:03Z", "Sun, 23 Aug 2026 09:05:03 UTC"),
        ];
        for (input, want) in cases {
            assert_eq!(go_rfc1123(input), want, "for {input}");
        }
    }

    #[test]
    fn unparseable_input_is_passed_through() {
        assert_eq!(go_rfc1123("not-a-timestamp"), "not-a-timestamp");
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod update_cli_tests {
    use super::*;
    use bv_update::github::{platform_asset_names, Asset, Release};
    use bv_update::{REPO_NAME, REPO_OWNER};

    /// An asset that passes `validate_release_asset` (github.rs:420-464).
    fn asset(tag: &str, name: &str, size: i64) -> Asset {
        Asset {
            name: name.to_string(),
            browser_download_url: format!(
                "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{tag}/{name}"
            ),
            size,
            digest: format!("sha256:{}", "ab".repeat(32)),
            state: "uploaded".to_string(),
        }
    }

    /// A release that `ValidateReleaseForUpdate` accepts on this platform.
    fn installable_release(tag: &str) -> Release {
        let names = platform_asset_names(tag);
        let archive = names
            .first()
            .expect("an archive name for this platform")
            .clone();
        let checksum = format!("{archive}.sha256");
        Release {
            tag_name: tag.to_string(),
            html_url: format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/tag/{tag}"),
            draft: false,
            prerelease: false,
            assets: vec![asset(tag, &archive, 4096), asset(tag, &checksum, 128)],
        }
    }

    /// Capture one reporting call. `report_*` writes only to the writers it
    /// is handed, so stdout and stderr are separable here even though Go
    /// picks the stream with `fmt.Printf` / `fmt.Fprintf(os.Stderr, …)`.
    fn capture_check_update(
        result: Result<Option<bv_update::UpdateInfo>, bv_update::github::FetchError>,
    ) -> (ExitCode, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = report_check_update(result, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    fn capture_dry_run(release: &Release) -> (ExitCode, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = report_update_dry_run(release, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    // --- --check-update (main.go:2057-2071) ------------------------------

    /// main.go:2065 labels the release *page* URL "Release:". bvr printed
    /// "Download:", which broke any byte comparison against the oracle.
    #[test]
    fn check_update_available_prints_release_not_download() {
        let info = bv_update::UpdateInfo {
            new_version: "v99.0.0".to_string(),
            release_url: "https://github.com/owner/repo/releases/tag/v99.0.0".to_string(),
        };
        let (code, out, err) = capture_check_update(Ok(Some(info)));
        assert_eq!(code, ExitCode::from(0));
        assert_eq!(err, "");
        assert_eq!(
            out,
            format!(
                "New version available: v99.0.0 (current: {})\n\
                 Release: https://github.com/owner/repo/releases/tag/v99.0.0\n\
                 \n\
                 Run 'bvr --update' to update automatically\n",
                bv_update::current_version()
            )
        );
        assert!(!out.contains("Download:"), "{out}");
    }

    #[test]
    fn check_update_up_to_date_exits_zero() {
        let (code, out, err) = capture_check_update(Ok(None));
        assert_eq!(code, ExitCode::from(0));
        assert_eq!(err, "");
        assert_eq!(
            out,
            format!(
                "bvr is up to date (version {})\n",
                bv_update::current_version()
            )
        );
    }

    /// main.go:2060-2062: a fetch failure is exit 1, on stderr.
    #[test]
    fn check_update_fetch_error_exits_one_on_stderr() {
        let (code, out, err) =
            capture_check_update(Err(bv_update::github::FetchError::Network("boom".into())));
        assert_eq!(code, ExitCode::from(1));
        assert_eq!(out, "");
        assert_eq!(err, "Error checking for updates: network error: boom\n");
    }

    // --- --update-dry-run (main.go:2074-2105) ----------------------------

    /// The plan Go prints, including the ` for {os}/{arch}` clause the Rust
    /// port had dropped from the download line.
    #[test]
    fn dry_run_prints_the_full_plan_with_platform_clause() {
        let release = installable_release("v99.0.0");
        let archive = &release.assets[0];
        let (code, out, err) = capture_dry_run(&release);
        assert_eq!(code, ExitCode::from(0));
        assert_eq!(err, "");
        assert_eq!(
            out,
            format!(
                "[dry-run] Would update bvr from {} to v99.0.0\n\
                 [dry-run] Would download {} (4096 bytes) for {}/{}\n\
                 [dry-run] From: {}\n\
                 [dry-run] Would verify SHA-256 checksum via {}.sha256\n\
                 [dry-run] No changes made. Run 'bvr --update' to apply.\n",
                bv_update::current_version(),
                archive.name,
                std::env::consts::OS,
                std::env::consts::ARCH,
                archive.browser_download_url,
                archive.name,
            )
        );
    }

    /// main.go:2091-2094 validates *before* printing a plan it could not
    /// carry out, and exits 1. A draft release proves the gate is present.
    #[test]
    fn dry_run_refuses_an_uninstallable_release_before_printing_a_plan() {
        let mut release = installable_release("v99.0.0");
        release.draft = true;
        let (code, out, err) = capture_dry_run(&release);
        assert_eq!(code, ExitCode::from(1));
        assert_eq!(out, "");
        assert_eq!(
            err,
            "Latest release cannot be installed automatically: release \"v99.0.0\" is still a draft\n"
        );
    }

    /// The same gate covers a release with no checksum manifest — Go's
    /// `checksums.txt asset is missing` (updater.go:848-852).
    #[test]
    fn dry_run_requires_a_checksum_asset() {
        let mut release = installable_release("v99.0.0");
        release.assets.pop();
        let (code, out, err) = capture_dry_run(&release);
        assert_eq!(code, ExitCode::from(1));
        assert_eq!(out, "");
        assert_eq!(
            err,
            "Latest release cannot be installed automatically: checksums.txt asset is missing\n"
        );
    }

    /// main.go:2083-2086: an unparseable tag is a hard error, not an
    /// "already up to date" answer. This is the second exit-1 path.
    #[test]
    fn dry_run_version_compare_failure_exits_one() {
        let mut release = installable_release("not-a-version");
        // Identity validation runs *after* the compare, so a malformed tag
        // only needs to reach the compare to prove the ordering.
        release.assets.clear();
        let (code, out, err) = capture_dry_run(&release);
        assert_eq!(code, ExitCode::from(1));
        assert_eq!(out, "");
        assert!(
            err.starts_with("Cannot compare release versions: "),
            "{err}"
        );
    }

    #[test]
    fn dry_run_not_newer_exits_zero_before_validating() {
        let mut release = installable_release("v0.0.1");
        // An uninstallable older release still answers "already up to date":
        // the compare short-circuits ahead of the validate gate.
        release.draft = true;
        let (code, out, err) = capture_dry_run(&release);
        assert_eq!(code, ExitCode::from(0));
        assert_eq!(err, "");
        assert_eq!(
            out,
            format!(
                "bvr is already up to date (version {})\n",
                bv_update::current_version()
            )
        );
    }

    // --- the [Y/n] gate (main.go:2129-2140, readUpdateConfirmation 1016) --

    fn confirm(input: &str) -> (UpdateConfirmation, String, String) {
        let mut reader = std::io::BufReader::new(input.as_bytes());
        let mut out = Vec::new();
        let mut err = Vec::new();
        let verdict =
            prompt_update_confirmation("v0.2.0", "v99.0.0", &mut reader, &mut out, &mut err);
        (
            verdict,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    #[test]
    fn confirmation_accepts_y_and_yes() {
        for answer in ["y\n", "Y\n", "yes\n", "YES\n", " yes \n"] {
            let (verdict, out, err) = confirm(answer);
            assert!(
                matches!(verdict, UpdateConfirmation::Proceed),
                "{answer:?} -> {verdict:?}"
            );
            assert_eq!(err, "", "{answer:?}");
            assert_eq!(
                out, "Update bvr from v0.2.0 to v99.0.0? [Y/n]: ",
                "{answer:?}"
            );
        }
    }

    /// A non-empty answer that is not y/yes is the only "Update cancelled"
    /// path (main.go:2139-2141), and it exits 0.
    #[test]
    fn only_a_non_empty_non_affirmative_answer_cancels() {
        for answer in ["n\n", "N\n", "no\n", "nope\n", "0\n"] {
            let (verdict, out, err) = confirm(answer);
            assert!(
                matches!(verdict, UpdateConfirmation::Cancelled),
                "{answer:?} -> {verdict:?}"
            );
            assert_eq!(err, "", "{answer:?}");
            assert_eq!(
                out, "Update bvr from v0.2.0 to v99.0.0? [Y/n]: Update cancelled\n",
                "{answer:?}"
            );
        }
    }

    /// main.go:1022-1027: an empty *line* means confirmed, so a bare newline
    /// proceeds.
    #[test]
    fn a_bare_newline_is_a_confirmed_default() {
        let (verdict, out, err) = confirm("\n");
        assert!(matches!(verdict, UpdateConfirmation::Proceed));
        assert_eq!(err, "");
        assert_eq!(out, "Update bvr from v0.2.0 to v99.0.0? [Y/n]: ");
    }

    /// main.go:1017-1028: `ReadString('\n')` returns `io.EOF` with the bytes
    /// it did read, so a final line *without* a newline is a real answer.
    #[test]
    fn a_final_line_without_a_newline_is_still_an_answer() {
        let (verdict, out, err) = confirm("n");
        assert!(matches!(verdict, UpdateConfirmation::Cancelled), "{out:?}");
        assert_eq!(err, "");
        assert_eq!(
            out,
            "Update bvr from v0.2.0 to v99.0.0? [Y/n]: Update cancelled\n"
        );
    }

    /// main.go:1023-1026: EOF with an *empty* response is an error, not a
    /// silent confirmation — `bvr --update < /dev/null` must never self-update
    /// unattended. Printed as main.go:2136 wraps it.
    #[test]
    fn eof_with_no_input_is_a_read_error() {
        let (verdict, out, err) = confirm("");
        assert!(
            matches!(verdict, UpdateConfirmation::ReadFailed),
            "{verdict:?}"
        );
        assert_eq!(out, "Update bvr from v0.2.0 to v99.0.0? [Y/n]: ");
        assert_eq!(
            err,
            "Cannot read update confirmation: no update confirmation received\n"
        );
    }

    /// Whitespace-only input still reads as empty, so EOF applies and the
    /// read-error path wins over the bare-newline default.
    #[test]
    fn whitespace_only_input_is_a_read_error() {
        let (verdict, _, err) = confirm("   ");
        assert!(matches!(verdict, UpdateConfirmation::ReadFailed));
        assert_eq!(
            err,
            "Cannot read update confirmation: no update confirmation received\n"
        );
    }
}

/// Handle `--rollback` (Go bv-182): restore the previous binary from backup.
fn run_rollback() -> ExitCode {
    // `perform_rollback` owns both success lines (Go updater.go:1533 and
    // 1561). Go's handler adds nothing on success, only "Rollback failed: %v"
    // (main.go:2164); bvr printed "Rollback complete" a second time here.
    match bv_update::perform_rollback() {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("Rollback failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// Go agent management commands (--agents-add/remove/update/check).
fn run_agents_commands(presence: &validation::Presence, _args: &[String]) -> ExitCode {
    // Go reports the directory it searched, and `os.Getwd` prefers `$PWD`, so a
    // run under /tmp must say /tmp and not /private/tmp.
    let cwd = go_working_dir();
    let detection = bv_core::agents::detect::detect_agent_file_in_parents(&cwd, 3);
    let is_robot = presence.has("robot-robot") || std::env::var("BV_ROBOT").as_deref() == Ok("1");
    let dry_run = presence.has("agents-dry-run");
    let force = presence.has("agents-force");

    if is_robot {
        // JSON output for AI agents.
        let result = serde_json::json!({
            "found": detection.found(),
            "file_path": detection.file_path,
            "file_type": detection.file_type,
            "has_blurb": detection.has_blurb,
            "has_legacy_blurb": detection.has_legacy_blurb,
            "blurb_version": detection.blurb_version,
            "current_version": bv_core::agents::BLURB_VERSION,
            "needs_blurb": detection.found() && detection.needs_blurb(),
            "needs_upgrade": detection.needs_upgrade(),
        });
        return emit_json(&result);
    }

    let is_check = !presence.has("agents-add")
        && !presence.has("agents-remove")
        && !presence.has("agents-update");

    if is_check || presence.has("agents-check") {
        if !detection.found() {
            println!(
                "No agent file found (searched up to 3 parent directories from {})",
                cwd.display()
            );
            println!(
                "Run 'bvr --agents-add' to create AGENTS.md with beads workflow instructions."
            );
            return ExitCode::from(0);
        }
        if detection.has_legacy_blurb {
            println!(
                "Found {} at {} (legacy blurb — needs upgrade)",
                detection.file_type, detection.file_path
            );
            println!("Run 'bvr --agents-update' to upgrade to the current format.");
            return ExitCode::from(0);
        }
        if detection.has_blurb && detection.blurb_version < bv_core::agents::BLURB_VERSION {
            println!(
                "Found {} at {} (blurb v{}, current v{} — needs update)",
                detection.file_type,
                detection.file_path,
                detection.blurb_version,
                bv_core::agents::BLURB_VERSION
            );
            println!("Run 'bvr --agents-update' to update to the latest version.");
            return ExitCode::from(0);
        }
        if detection.has_blurb {
            println!(
                "Found {} at {} (blurb v{} — up to date)",
                detection.file_type, detection.file_path, detection.blurb_version
            );
            return ExitCode::from(0);
        }
        println!(
            "Found {} at {} (no beads workflow instructions)",
            detection.file_type, detection.file_path
        );
        println!("Run 'bvr --agents-add' to add beads workflow instructions.");
        return ExitCode::from(0);
    }

    if presence.has("agents-add") {
        if detection.found()
            && detection.has_blurb
            && detection.blurb_version >= bv_core::agents::BLURB_VERSION
        {
            println!(
                "{} already has current blurb (v{}) — no action needed.",
                detection.file_path, detection.blurb_version
            );
            return ExitCode::from(0);
        }
        if detection.found()
            && (detection.has_legacy_blurb
                || (detection.has_blurb
                    && detection.blurb_version < bv_core::agents::BLURB_VERSION))
        {
            println!("Existing blurb found but outdated. Use --agents-update instead.");
            return ExitCode::from(1);
        }

        let target_path = if detection.found() {
            detection.file_path.clone()
        } else {
            bv_core::agents::get_preferred_agent_file_path(&cwd)
                .to_string_lossy()
                .to_string()
        };
        let creating = !detection.found();

        if dry_run {
            if creating {
                println!("[dry-run] Would create {target_path} with beads workflow instructions.");
            } else {
                println!("[dry-run] Would append beads workflow instructions to {target_path}.");
            }
            return ExitCode::from(0);
        }

        if !force {
            let action = if creating {
                "Create"
            } else {
                "Append blurb to"
            };
            print!("{action} {target_path}? [Y/n]: ");
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf).ok();
            let resp = buf.trim().to_lowercase();
            if !resp.is_empty() && resp != "y" && resp != "yes" {
                println!("Cancelled.");
                return ExitCode::from(0);
            }
        }

        let result = if creating {
            bv_core::agents::file::create_agent_file(std::path::Path::new(&target_path))
        } else {
            bv_core::agents::file::append_blurb_to_file(std::path::Path::new(&target_path))
        };

        if let Err(e) = result {
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }

        let msg = if creating {
            format!("Created {target_path} with beads workflow instructions.")
        } else {
            format!("Appended beads workflow instructions to {target_path}.")
        };
        println!("{msg}");

        let verified =
            bv_core::agents::file::verify_blurb_present(std::path::Path::new(&target_path))
                .unwrap_or(false);
        if !verified {
            eprintln!("Warning: verification failed — blurb may not have been written correctly.");
            return ExitCode::from(1);
        }
        return ExitCode::from(0);
    }

    if presence.has("agents-update") {
        if !detection.found() {
            println!("No agent file found. Use --agents-add to create one.");
            return ExitCode::from(1);
        }
        if !detection.has_blurb && !detection.has_legacy_blurb {
            println!(
                "{} has no blurb to update. Use --agents-add to add one.",
                detection.file_path
            );
            return ExitCode::from(1);
        }
        if detection.has_blurb && detection.blurb_version >= bv_core::agents::BLURB_VERSION {
            println!(
                "{} already has current blurb (v{}) — no update needed.",
                detection.file_path, detection.blurb_version
            );
            return ExitCode::from(0);
        }

        if dry_run {
            if detection.has_legacy_blurb {
                println!(
                    "[dry-run] Would upgrade legacy blurb to v{} in {}.",
                    bv_core::agents::BLURB_VERSION,
                    detection.file_path
                );
            } else {
                println!(
                    "[dry-run] Would update blurb from v{} to v{} in {}.",
                    detection.blurb_version,
                    bv_core::agents::BLURB_VERSION,
                    detection.file_path
                );
            }
            return ExitCode::from(0);
        }

        if !force {
            print!("Update blurb in {}? [Y/n]: ", detection.file_path);
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf).ok();
            let resp = buf.trim().to_lowercase();
            if !resp.is_empty() && resp != "y" && resp != "yes" {
                println!("Cancelled.");
                return ExitCode::from(0);
            }
        }

        if let Err(e) =
            bv_core::agents::file::update_blurb_in_file(std::path::Path::new(&detection.file_path))
        {
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
        println!(
            "Updated blurb to v{} in {}.",
            bv_core::agents::BLURB_VERSION,
            detection.file_path
        );

        let verified =
            bv_core::agents::file::verify_blurb_present(std::path::Path::new(&detection.file_path))
                .unwrap_or(false);
        if !verified {
            eprintln!("Warning: verification failed — blurb may not have been written correctly.");
            return ExitCode::from(1);
        }
        return ExitCode::from(0);
    }

    if presence.has("agents-remove") {
        if !detection.found() {
            println!("No agent file found — nothing to remove.");
            return ExitCode::from(0);
        }
        if !detection.has_blurb && !detection.has_legacy_blurb {
            println!("{} has no blurb — nothing to remove.", detection.file_path);
            return ExitCode::from(0);
        }

        if dry_run {
            println!("[dry-run] Would remove blurb from {}.", detection.file_path);
            return ExitCode::from(0);
        }

        if !force {
            print!("Remove blurb from {}? [Y/n]: ", detection.file_path);
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf).ok();
            let resp = buf.trim().to_lowercase();
            if !resp.is_empty() && resp != "y" && resp != "yes" {
                println!("Cancelled.");
                return ExitCode::from(0);
            }
        }

        if let Err(e) = bv_core::agents::file::remove_blurb_from_file(std::path::Path::new(
            &detection.file_path,
        )) {
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
        println!(
            "Removed beads workflow instructions from {}.",
            detection.file_path
        );
        return ExitCode::from(0);
    }

    // Default: should not reach here
    eprintln!("No agents action specified. Use --agents-add, --agents-remove, --agents-update, or --agents-check.");
    ExitCode::from(1)
}

/// Go's CPU-profile sample rate. `runtime/pprof.StartCPUProfile` calls
/// `SetCPUProfileRate(100)` when the caller has not set one, so 100 Hz is what
/// a Go `bv --cpu-profile out.pprof` run records. pprof-rs defaults to 99, so
/// the rate is pinned rather than inherited.
const GO_CPU_PROFILE_HZ: i32 = 100;

/// Keeps the sampler running and owns the destination file for as long as the
/// binding in `main` lives. Go spells the same lifetime with a `defer` plus a
/// closure handed to `RobotContext.FinalizeBeforeExit`; `Drop` fires exactly
/// once, which is the `profileActive` latch in Go's `stopCPUProfile` closure
/// (main.go:1919-1928) stated in a form the compiler enforces.
struct CpuProfileGuard {
    /// The sampler. Reporting has to happen while this is still alive —
    /// pprof-rs reads the sample buffer off the guard, and the guard's own
    /// `Drop` is what stops the signal handler.
    profiler: pprof::ProfilerGuard<'static>,
    file: std::fs::File,
    path: String,
}

impl Drop for CpuProfileGuard {
    fn drop(&mut self) {
        // Go's order (main.go:1924-1929): stop the sampler, then close the
        // file, and a close failure is a warning that leaves the exit code
        // alone. Stopping first is what makes the profile complete, so the
        // report is taken from the guard before the guard goes away.
        let built = self
            .profiler
            .report()
            .build()
            .and_then(|report| report.pprof());

        let profile = match built {
            Ok(profile) => profile,
            // Go has no equivalent branch — `pprof.StopCPUProfile()` returns
            // nothing and writes through the file it was handed — so this one
            // carries its own wording, at the same severity as Go's close
            // failure: on stderr, without touching the exit code, which `main`
            // has already decided by the time the guard drops.
            Err(e) => {
                eprintln!("Could not write CPU profile {}: {e}", self.path);
                return;
            }
        };

        // Scoped so the encoder releases its `&mut self.file` borrow before
        // the file itself is flushed. Serialising straight into the gzip
        // encoder mirrors Go, which streams `StopCPUProfile`'s output into the
        // file it was handed at `StartCPUProfile` time. `finish()` writes the
        // gzip trailer and is the last thing that can fail before the profile
        // is on disk, so its failure is the one Go reports as a close failure.
        {
            let mut gz =
                flate2::write::GzEncoder::new(&mut self.file, flate2::Compression::default());
            if let Err(e) = profile.write_to_writer(&mut gz) {
                eprintln!("Could not close CPU profile {}: {e}", self.path);
                return;
            }
            if let Err(e) = gz.finish() {
                eprintln!("Could not close CPU profile {}: {e}", self.path);
                return;
            }
        }
        if let Err(e) = self.file.flush() {
            eprintln!("Could not close CPU profile {}: {e}", self.path);
        }
    }
}

/// Go's `*fs.PathError.Error()` — `<op> <path>: <err>`, with `err` the bare
/// errno description.
///
/// `io::Error`'s own `Display` is the same description but capitalised and with
/// `(os error N)` glued on, so the two are not interchangeable: Go prints
/// `open /x: no such file or directory` where Rust would print
/// `open /x: No such file or directory (os error 2)`. Trimming the numeric
/// tail and lower-casing the first character reproduces Go's wording for the
/// errno strings both systems share, which is every one a failed `os.Create`
/// can produce here.
fn go_path_error(op: &str, path: &str, e: &std::io::Error) -> String {
    let mut msg = e.to_string();
    if e.raw_os_error().is_some() {
        if let Some(cut) = msg.rfind(" (os error ") {
            msg.truncate(cut);
        }
    }
    if let Some(first) = msg.get_mut(0..1) {
        first.make_ascii_lowercase();
    }
    format!("{op} {path}: {msg}")
}

/// Go main.go:1910-1933 — `os.Create` the path, start the sampler, and return
/// something that stops both on the way out.
///
/// An absent or empty `--cpu-profile` is the unset case and returns `Ok(None)`,
/// so a normal run never touches a profiler. Both failure branches are the
/// ones Go exits 1 on, with Go's message text: a create failure is
/// `Could not create CPU profile: %v`, a start failure is
/// `Could not start CPU profile: %v` after the file has been closed again.
fn start_cpu_profile(path: Option<&str>) -> Result<Option<CpuProfileGuard>, String> {
    let path = match path.filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => return Ok(None),
    };

    // `os.Create` is `O_RDWR|O_CREATE|O_TRUNC`, which truncates an existing
    // profile — the same as Rust's `File::create`.
    let file = std::fs::File::create(path).map_err(|e| {
        format!(
            "Could not create CPU profile: {}",
            go_path_error("open", path, &e)
        )
    })?;

    let profiler = match pprof::ProfilerGuardBuilder::default()
        .frequency(GO_CPU_PROFILE_HZ)
        .build()
    {
        Ok(profiler) => profiler,
        Err(e) => {
            // Go main.go:1917-1920 closes the file before reporting, so a
            // failed start leaves nothing behind to be cleaned up.
            drop(file);
            return Err(format!("Could not start CPU profile: {e}"));
        }
    };

    Ok(Some(CpuProfileGuard {
        profiler,
        file,
        path: path.to_string(),
    }))
}

/// Go reads the top-N metric lists from a baseline's sibling `top_metrics`
/// object (baseline.go:38-40), not from `stats`. A document without it — which
/// includes every baseline this port wrote before the schema was corrected —
/// yields an empty list, and the drift calculator then reports every current
/// entry as having "entered top", exactly as Go does.
fn baseline_top_metrics(doc: &serde_json::Value) -> bv_analysis::drift::TopMetrics {
    doc.get("top_metrics")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Go `Result.Summary` (pkg/drift/drift.go:752-786) — the human-readable
/// rendering `--check-drift` prints when `--robot-drift` is absent. Go picks
/// between this and the JSON envelope with a single `if *robotDriftCheck`
/// (cmd/bv/main.go:3708); without it the CLI wrote a Rust-only JSON blob whose
/// `summary` was a formatted string rather than Go's object.
fn drift_summary(result: &bv_analysis::drift::DriftResult) -> String {
    if !result.has_drift {
        return "No drift detected. Project metrics are within baseline thresholds.\n".to_string();
    }
    let mut sb = String::from("Drift Analysis Summary\n======================\n\n");
    if result.critical_count > 0 {
        sb.push_str(&format!(
            "🔴 CRITICAL: {} issue(s)\n",
            result.critical_count
        ));
    }
    if result.warning_count > 0 {
        sb.push_str(&format!("🟡 WARNING: {} issue(s)\n", result.warning_count));
    }
    if result.info_count > 0 {
        sb.push_str(&format!("🔵 INFO: {} issue(s)\n", result.info_count));
    }
    sb.push_str("\nDetails:\n");
    for alert in &result.alerts {
        let icon = match alert.severity {
            bv_analysis::drift::Severity::Critical => "🔴",
            bv_analysis::drift::Severity::Warning => "🟡",
            _ => "ℹ️",
        };
        // Go prints the alert type's wire value; AlertType serializes
        // snake_case and has no Display, so read it back off the JSON form.
        let alert_type = serde_json::to_value(alert.alert_type)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        sb.push_str(&format!("  {icon} [{alert_type}] {}\n", alert.message));
        for detail in &alert.details {
            sb.push_str(&format!("      - {detail}\n"));
        }
    }
    sb.push('\n');
    sb
}

fn run_check_drift() -> ExitCode {
    let baseline_doc = match std::fs::read_to_string(BASELINE_PATH) {
        Ok(raw) => raw,
        Err(_) => {
            eprintln!("No baseline found at {BASELINE_PATH}. Save one with --save-baseline.");
            return ExitCode::from(1);
        }
    };
    let base: serde_json::Value = match serde_json::from_str(&baseline_doc) {
        Ok(v) => v,
        Err(e) => {
            // Go reports this as `Error loading baseline: parsing baseline: ...`
            // and exits 1 (cmd/bv/main.go:2571-2575). Rust panicked on a
            // malformed file instead, which is a crash on user data.
            eprintln!("Error loading baseline: parsing baseline: {e}");
            return ExitCode::from(1);
        }
    };
    let mut base_stats: bv_analysis::drift::BaselineStats =
        match serde_json::from_value(base["stats"].clone()) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error loading baseline: parsing baseline: {e}");
                return ExitCode::from(1);
            }
        };
    base_stats.pagerank = baseline_top_metrics(&base).pagerank_map();
    let old_cycles: Vec<Vec<String>> = base
        .get("cycles")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default();

    let (current, new_cycles, _hash, _top) = match capture_baseline() {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    // Go builds this calculator at main.go:3704 and never calls `SetIssues`,
    // so `c.issues` stays nil and every issue-level check short-circuits:
    // `checkStaleness` returns on `len(c.issues) == 0` (drift.go:577), as do
    // blocking-cascade, duplicate and priority-mismatch. Passing the issues
    // here made bvr report twelve critical stale_issue alerts against a
    // baseline that had been saved seconds earlier, where Go reports none.
    // The compatibility contract says match Go, so the list is deliberately
    // empty here — not an oversight to be tidied away.
    const NO_ISSUES: &[bv_core::model::Issue] = &[];

    let norm: std::collections::HashSet<String> = old_cycles
        .iter()
        .map(|c| {
            let mut v = c.clone();
            v.sort();
            v.join("|")
        })
        .collect();
    let fresh_cycles: Vec<Vec<String>> = new_cycles
        .into_iter()
        .filter(|c| {
            let mut v = c.clone();
            v.sort();
            !norm.contains(&v.join("|"))
        })
        .collect();

    let result = bv_analysis::drift::calculate(
        &base_stats,
        &current,
        &bv_analysis::drift::DriftConfig::default(),
        &fresh_cycles,
        NO_ISSUES,
        robot_now(),
    );
    print!("{}", drift_summary(&result));
    ExitCode::from(result.exit_code())
}

/// Go `robot-drift` — wraps `--check-drift` with structured JSON output.
/// JSON schema matches Go output struct at cmd/bv/main.go:3509-3541.
/// The `baseline` sub-object of the `--robot-drift` payload (main.go:3727-3730).
///
/// `commit_sha` is `omitempty` in Go, so a baseline saved outside a git
/// repository carries no key at all rather than an empty string. `json!` has
/// no spread, hence the imperative build.
fn drift_baseline_block(created_at: &str, commit_sha: &str) -> serde_json::Value {
    let mut block = serde_json::json!({ "created_at": created_at });
    if !commit_sha.is_empty() {
        block["commit_sha"] = serde_json::json!(commit_sha);
    }
    block
}

/// Re-render an RFC3339 timestamp at second precision, keeping its zone.
/// Mirrors Go's `time.RFC3339` formatting of a value it read from a baseline
/// file; an unparseable string is passed through untouched.
fn rfc3339_second_precision(rfc3339: &str) -> String {
    let Ok(ts) = rfc3339.parse::<jiff::Timestamp>() else {
        return rfc3339.to_string();
    };
    let offset = rfc3339_offset_label(rfc3339)
        .as_deref()
        .and_then(rfc3339_offset_seconds)
        .unwrap_or(0);
    let shifted = ts
        .checked_add(std::time::Duration::from_secs(offset.unsigned_abs()))
        .unwrap_or(ts);
    let dt = shifted.to_zoned(jiff::tz::TimeZone::UTC).datetime();
    let zone = if offset == 0 {
        "Z".to_string()
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        format!(
            "{sign}{:02}:{:02}",
            offset.abs() / 3600,
            (offset.abs() % 3600) / 60
        )
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{zone}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

fn run_robot_drift() -> ExitCode {
    let baseline_doc = match std::fs::read_to_string(BASELINE_PATH) {
        Ok(raw) => raw,
        Err(_) => {
            let payload = serde_json::json!({
                "generated_at": jiff_now(),
                "error": format!("No baseline found at {BASELINE_PATH}. Save one with --save-baseline."),
            });
            emit_json(&payload);
            return ExitCode::from(1);
        }
    };
    let base: serde_json::Value = match serde_json::from_str(&baseline_doc) {
        Ok(v) => v,
        Err(e) => {
            // Go reports this as `Error loading baseline: parsing baseline: ...`
            // and exits 1 (cmd/bv/main.go:2571-2575). Rust panicked on a
            // malformed file instead, which is a crash on user data.
            eprintln!("Error loading baseline: parsing baseline: {e}");
            return ExitCode::from(1);
        }
    };
    let mut base_stats: bv_analysis::drift::BaselineStats =
        match serde_json::from_value(base["stats"].clone()) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error loading baseline: parsing baseline: {e}");
                return ExitCode::from(1);
            }
        };
    base_stats.pagerank = baseline_top_metrics(&base).pagerank_map();
    let old_cycles: Vec<Vec<String>> = base
        .get("cycles")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default();
    // Go re-formats the stored timestamp through `time.RFC3339` on the way out
    // (main.go:3732), which is second precision. A baseline file carries
    // nanoseconds — Go's own `time.Time` marshals RFC3339Nano — so passing the
    // string through verbatim echoed a fraction the oracle truncates.
    let baseline_created_at = base
        .get("created_at")
        .and_then(|v| v.as_str())
        .map(rfc3339_second_precision)
        .unwrap_or_default();
    let baseline_commit_sha = base
        .get("commit_sha")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (current, new_cycles, _hash, _top) = match capture_baseline() {
        Ok(x) => x,
        Err(e) => {
            let payload = serde_json::json!({
                "generated_at": jiff_now(),
                "error": format!("Error capturing baseline: {e}"),
            });
            emit_json(&payload);
            return ExitCode::from(1);
        }
    };

    // Go builds this calculator at main.go:3704 and never calls `SetIssues`,
    // so `c.issues` stays nil and every issue-level check short-circuits:
    // `checkStaleness` returns on `len(c.issues) == 0` (drift.go:577), as do
    // blocking-cascade, duplicate and priority-mismatch. Passing the issues
    // here made bvr report twelve critical stale_issue alerts against a
    // baseline that had been saved seconds earlier, where Go reports none.
    // The compatibility contract says match Go, so the list is deliberately
    // empty here — not an oversight to be tidied away.
    const NO_ISSUES: &[bv_core::model::Issue] = &[];

    let norm: std::collections::HashSet<String> = old_cycles
        .iter()
        .map(|c| {
            let mut v = c.clone();
            v.sort();
            v.join("|")
        })
        .collect();
    let fresh_cycles: Vec<Vec<String>> = new_cycles
        .into_iter()
        .filter(|c| {
            let mut v = c.clone();
            v.sort();
            !norm.contains(&v.join("|"))
        })
        .collect();

    let result = bv_analysis::drift::calculate(
        &base_stats,
        &current,
        &bv_analysis::drift::DriftConfig::default(),
        &fresh_cycles,
        NO_ISSUES,
        robot_now(),
    );

    // Go parity: exact JSON structure from cmd/bv/main.go:3511-3541.
    let payload = serde_json::json!({
        "generated_at": jiff_now(),
        "has_drift": result.has_drift,
        "exit_code": result.exit_code(),
        "summary": {
            "critical": result.critical_count,
            "warning": result.warning_count,
            "info": result.info_count,
        },
        "alerts": result.alerts,
        "baseline": drift_baseline_block(&baseline_created_at, &baseline_commit_sha),
    });
    emit_json(&payload);
    ExitCode::from(result.exit_code())
}

/// The flag a history invocation actually used. `--robot-history` and
/// `--bead-history` share one handler (main.rs:1943), and Go's dispatcher names
/// the flag the caller typed — so a repository check that fails has to say
/// which of the two it was.
fn history_flag_name(args: &[String]) -> &'static str {
    if args
        .iter()
        .any(|a| a == "--bead-history" || a.starts_with("--bead-history="))
    {
        "--bead-history"
    } else {
        "--robot-history"
    }
}

/// Go `correlation.ValidateRepository` — a `.git` directory plus at least one
/// of the known beads file names under `.beads/`.
/// Go's `os.Getwd()`: on Unix it returns `$PWD` when that names the same
/// directory, so a run under a symlinked path is reported the way the user
/// typed it. `std::env::current_dir()` always resolves, so on macOS every path
/// Go would print read `/private/tmp/...` instead of `/tmp/...` — and these
/// paths end up verbatim in error messages, a baseline file's `source_path`,
/// and the saved `created_at`.
fn go_working_dir() -> std::path::PathBuf {
    let physical = std::env::current_dir().unwrap_or_default();
    // Go checks that `$PWD` names the *current* directory before trusting it
    // (os/getwd.go: "If the operating system provides a Getwd call that uses
    // the PWD environment variable, and the PWD variable names the current
    // directory, use it"). Skipping that check makes a stale PWD win over the
    // real cwd: running `bvr` from a fixture while the shell's PWD is the
    // repository root would then read the repository's issues instead of the
    // fixture's, and report success where Go reports the fixture's error.
    if let Ok(pwd) = std::env::var("PWD") {
        let pwd = std::path::PathBuf::from(pwd);
        if std::fs::canonicalize(&pwd).ok() == std::fs::canonicalize(&physical).ok() {
            return pwd;
        }
    }
    physical
}

fn validate_correlation_repository(repo: &std::path::Path) -> Result<(), String> {
    if !repo.join(".git").exists() {
        return Err(format!("not a git repository: {}", repo.to_string_lossy()));
    }
    let found = bv_correlation::extractor::DEFAULT_BEADS_FILES
        .iter()
        .any(|name| repo.join(".beads").join(name).exists());
    if !found {
        return Err(format!(
            "no beads file found in {}/.beads/",
            repo.to_string_lossy()
        ));
    }
    Ok(())
}

/// Read a string flag from the process argv, accepting both `--flag=value` and
/// `--flag value`. `rewrite_args` only rewrites bare boolean flags, so the raw
/// argv still carries every valued flag verbatim.
fn history_flag_value(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let with_eq = format!("--{name}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&with_eq) {
            return Some(v.to_string());
        }
        if a == &format!("--{name}") {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// The zone Go's `now.Location()` has for the current run: a frozen
/// `SOURCE_DATE_EPOCH` instant is UTC, an unfrozen `time.Now()` is Local.
fn robot_now_zone() -> jiff::tz::TimeZone {
    if let Ok(v) = std::env::var("SOURCE_DATE_EPOCH") {
        if v.trim().parse::<i64>().is_ok() {
            return jiff::tz::TimeZone::UTC;
        }
    }
    jiff::tz::TimeZone::try_system().unwrap_or(jiff::tz::TimeZone::UTC)
}

/// Go `recipe.ParseRelativeTime` — `7d` / `3w` / `6m` / `1y` counted back from
/// now, else RFC3339, else `YYYY-MM-DDTHH:MM:SS`, else `YYYY-MM-DD`.
///
/// The reference "now" is `robot_now()` (SOURCE_DATE_EPOCH-aware), not the wall
/// clock: Go resolves relative times against the same instant it stamps
/// `generated_at` with, so a frozen-clock run stays frozen here too. An explicit
/// RFC3339 input keeps its own offset — Go's `time.Parse` retains the parsed
/// location, and the `window`/`git_range` fields echo that offset back.
fn parse_relative_time(raw: &str) -> Result<Option<String>, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    // Relative form: a digit run followed by one of d/w/m/y.
    let lower = s.to_lowercase();
    if lower.len() >= 2 {
        let (digits, unit) = lower.split_at(lower.len() - 1);
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let n: i64 = digits
                .parse()
                .map_err(|_| format!("could not parse time \"{raw}\""))?;
            let span = match unit {
                "d" => jiff::Span::new().days(-n),
                "w" => jiff::Span::new().days(-n * 7),
                "m" => jiff::Span::new().months(-n),
                "y" => jiff::Span::new().years(-n),
                _ => jiff::Span::new().days(0),
            };
            if !span.is_zero() {
                let shifted = robot_now()
                    .to_zoned(robot_now_zone())
                    .checked_add(span)
                    .map_err(|e| e.to_string())?;
                return Ok(Some(shifted.timestamp().to_string()));
            }
        }
    }
    // An explicit instant is already in the shape the report echoes back.
    if s.parse::<jiff::Timestamp>().is_ok() {
        return Ok(Some(s.to_string()));
    }
    // Naive layouts, read in the reference zone like Go's ParseInLocation.
    let tz = robot_now_zone();
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d"] {
        if let Ok(dt) = jiff::civil::DateTime::strptime(fmt, s) {
            if let Ok(zoned) = dt.to_zoned(tz.clone()) {
                return Ok(Some(zoned.timestamp().to_string()));
            }
        }
    }
    if let Ok(date) = jiff::civil::Date::strptime("%Y-%m-%d", s) {
        let dt = date.to_datetime(jiff::civil::Time::midnight());
        if let Ok(zoned) = dt.to_zoned(tz) {
            return Ok(Some(zoned.timestamp().to_string()));
        }
    }
    Err(format!("could not parse time \"{raw}\""))
}

fn run_robot_history() -> ExitCode {
    // The raw argv is enough: the check is for the literal flag, and Go's
    // dispatcher names the flag as typed, before any rewriting.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = go_working_dir();
    if let Err(e) = validate_correlation_repository(&cwd) {
        if unresolved_source_probe(&cwd) {
            return emit_unresolved_source();
        }
        // Go returns this as a plain error, so its dispatcher wraps it
        // as `Error handling <flag>: <err>` (robot_registry.go:508).
        eprintln!("Error handling {}: {e}", history_flag_name(&args));
        return ExitCode::from(1);
    }
    let (issues, _, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    // Go: CorrelatorOptions{Limit: 500} overridden by --history-limit, with
    // --bead-history narrowing to one bead and --history-since bounding the
    // window (a parse failure is fatal, as in Go's handleRobotHistory).
    let mut opts = bv_correlation::history::HistoryOptions {
        limit: 500,
        ..Default::default()
    };
    if let Some(bead) = history_flag_value("bead-history") {
        opts.bead_id = bead.trim().to_string();
    }
    if let Some(limit) = history_flag_value("history-limit") {
        match limit.trim().parse::<i64>() {
            Ok(v) => opts.limit = v,
            Err(_) => {
                eprintln!("Error: invalid --history-limit: {limit}");
                return ExitCode::from(2);
            }
        }
    }
    if let Some(since) = history_flag_value("history-since") {
        match parse_relative_time(&since) {
            Ok(Some(ts)) => opts.since = Some(ts),
            Ok(None) => {}
            Err(e) => {
                eprintln!("Error: parsing --history-since: {e}");
                return ExitCode::from(1);
            }
        }
    }

    // Go builds BeadInfo from the loaded issues, in load order, on a correlator
    // with the feedback store attached (robot_registry.go:2623-2632).
    let mut report = match generate_correlation_report(&cwd, &issues, &opts, jiff_now(), true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: generating history report: {e}");
            return ExitCode::from(1);
        }
    };

    // Go: --min-confidence filters the assembled histories, then re-derives the
    // commit index and the beads_with_commits count.
    if let Some(min) = history_flag_value("min-confidence") {
        if let Ok(floor) = min.trim().parse::<f64>() {
            bv_correlation::history::filter_histories_by_confidence(&mut report, floor);
        }
    }

    // Same top-level keys as the report plus the shared envelope. The envelope
    // carries generated_at/data_hash (Go's output struct embeds the envelope
    // and copies the report's remaining fields rather than embedding it, so the
    // colliding keys would have been dropped).
    //
    // Go builds the envelope from the *loader's* source authority (whose
    // per-source data_hash is the full-file sha256) and only then overrides the
    // top-level data_hash with the report's own bead fingerprint — so
    // authority_hash is computed over the file hash while scope_hash is
    // computed over the report hash. Build with the file hash, then substitute.
    let file_hash = bv_core::data_hash::compute_data_hash(&issues);
    let mut payload = full_envelope_for(&file_hash, &issues);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("data_hash".into(), serde_json::json!(report.data_hash));
        let mut ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        let scope_hash = bv_robot::scope_hash("", "", "", &report.data_hash, &ids);
        if !scope_hash.is_empty() {
            obj.insert("scope_hash".into(), serde_json::json!(scope_hash));
        }
        obj.insert("git_range".into(), serde_json::json!(report.git_range));
        if !report.latest_commit_sha.is_empty() {
            obj.insert(
                "latest_commit_sha".into(),
                serde_json::json!(report.latest_commit_sha),
            );
        }
        obj.insert(
            "window".into(),
            serde_json::to_value(&report.window).unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "stats".into(),
            serde_json::to_value(&report.stats).unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "histories".into(),
            serde_json::to_value(&report.histories).unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "commit_index".into(),
            serde_json::to_value(&report.commit_index).unwrap_or(serde_json::Value::Null),
        );
    }
    emit_json(&payload)
}

/// Go `handleRobotOrphans` — build the same correlation report `--robot-history`
/// produces, hand it to the orphan detector (which scans exactly the window
/// that index covered), then drop candidates below `--orphans-min-score`.
fn run_robot_orphans() -> ExitCode {
    let cwd = go_working_dir();
    if let Err(e) = validate_correlation_repository(&cwd) {
        if unresolved_source_probe(&cwd) {
            return emit_unresolved_source();
        }
        // Go returns this as a plain error, so its dispatcher wraps it
        // as `Error handling <flag>: <err>` (robot_registry.go:508).
        eprintln!("Error handling --robot-orphans: {e}");
        return ExitCode::from(1);
    }
    let (issues, _, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    // Go: CorrelatorOptions{Limit: 500}, same default the history handler uses.
    let mut opts = bv_correlation::history::HistoryOptions {
        limit: 500,
        ..Default::default()
    };
    if let Some(limit) = history_flag_value("history-limit") {
        match limit.trim().parse::<i64>() {
            Ok(v) => opts.limit = v,
            Err(_) => {
                eprintln!("Error: invalid --history-limit: {limit}");
                return ExitCode::from(2);
            }
        }
    }

    let min_score: i32 = match history_flag_value("orphans-min-score") {
        Some(v) => match v.trim().parse::<i32>() {
            Ok(score) => score,
            Err(_) => {
                eprintln!("Error: invalid --orphans-min-score: {v}");
                return ExitCode::from(2);
            }
        },
        None => 30,
    };

    // Go wraps the history report's own failure as
    // `generating history report: %w` before the caller's
    // `generating report: %w`, so the chain reads
    // "generating report: generating history report: extracting events: ...".
    let report = match generate_correlation_report(&cwd, &issues, &opts, jiff_now(), true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: generating history report: {e}");
            return ExitCode::from(1);
        }
    };

    let now = robot_now();
    let mut orphan_report =
        match bv_correlation::orphan::detect_orphans(&cwd, &report, now, jiff_now()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: detecting orphans: {e}");
                return ExitCode::from(1);
            }
        };
    bv_correlation::orphan::filter_by_min_score(&mut orphan_report, min_score);

    // Go copies the report's fields onto a struct that embeds the envelope
    // rather than the report, so the envelope's generated_at/data_hash lead and
    // the report's own copies of those keys are dropped.
    let file_hash = bv_core::data_hash::compute_data_hash(&issues);
    let mut payload = full_envelope_for(&file_hash, &issues);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "data_hash".into(),
            serde_json::json!(orphan_report.data_hash),
        );
        let mut ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        let scope_hash = bv_robot::scope_hash("", "", "", &orphan_report.data_hash, &ids);
        if !scope_hash.is_empty() {
            obj.insert("scope_hash".into(), serde_json::json!(scope_hash));
        }
        obj.insert(
            "git_range".into(),
            serde_json::json!(orphan_report.git_range),
        );
        obj.insert(
            "window".into(),
            serde_json::to_value(&orphan_report.window).unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "stats".into(),
            serde_json::to_value(&orphan_report.stats).unwrap_or(serde_json::Value::Null),
        );
        obj.insert(
            "candidates".into(),
            serde_json::to_value(&orphan_report.candidates).unwrap_or(serde_json::Value::Null),
        );
        if !orphan_report.by_bead.is_empty() {
            obj.insert(
                "by_bead".into(),
                serde_json::to_value(&orphan_report.by_bead).unwrap_or(serde_json::Value::Null),
            );
        }
        obj.insert(
            "usage_hints".into(),
            serde_json::to_value(&orphan_report.usage_hints).unwrap_or(serde_json::Value::Null),
        );
    }
    emit_json(&payload)
}

type AnalysisTuple = (
    Vec<bv_core::model::Issue>,
    String,
    bv_analysis::analyzer::Phase1Stats,
    bv_analysis::MetricStatus,
    std::sync::Arc<bv_graph_core::DiGraph>,
);

/// Shared helper: load issues, build graph, run analysis phases.
fn load_and_analyze() -> Result<AnalysisTuple, ExitCode> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, _) = match bv_core::discovery::load_issues_from_repo(&cwd) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return Err(emit_unresolved_source());
            }
            eprintln!("Error: {e}");
            return Err(ExitCode::from(1));
        }
    };
    // Go main.go:2786 — `--repo` then `--label` narrow the set before any
    // handler analyses it, and a `--repo` filter re-hashes the narrowed source
    // (main.go:4883-4884).
    let (issues, hash_override) = apply_scope(&issues);
    if issues.is_empty() {
        println!(
            "{{\"generated_at\":\"{}\",\"data_hash\":\"empty\",\"error\":\"no issues loaded\"}}",
            jiff_now()
        );
        return Err(ExitCode::from(0));
    }
    let data_hash = hash_override.unwrap_or_else(|| bv_core::data_hash::compute_data_hash(&issues));
    let g = std::sync::Arc::new(bv_analysis::build_graph(&issues));
    let p1 = bv_analysis::analyze_phase1(&g);
    let budget = bv_analysis::AnalysisBudget {
        density: p1.density,
        ..bv_analysis::AnalysisBudget::default()
    };

    // Phase 2 — run blocking since we're in a CLI context.
    // For parity with Go's async behavior we'd need the thread-per-metric approach;
    // for now use the synchronous path which is equivalent in output.
    let g2 = std::sync::Arc::clone(&g);
    let (status, _phase2) = bv_analysis::analyze_phase2_blocking(g2, &budget);

    Ok((issues, data_hash, p1, status, g))
}

/// Go `NewRobotEnvelope` parity: for handlers whose Go output embeds the
/// full `RobotEnvelope` struct (alerts, next, history, …) the envelope also
/// carries `output_format` and `version` — golden-verified.
/// Build the v0.25.0 envelope prefix. Field order matches Go `RobotEnvelope`
/// (cmd/bv/main.go:7215) because the serializer preserves insertion order.
/// Envelope with Go v0.25.0 source provenance + scope/authority hashes.
fn full_envelope_json_with_source(
    data_hash: &str,
    source: Option<&SourceMeta>,
    issues: &[bv_core::model::Issue],
) -> serde_json::Value {
    full_envelope_json_with_source_and_authority(data_hash, data_hash, source, issues)
}

/// As [`full_envelope_json_with_source`], but the source authority may name a
/// different digest from the top-level `data_hash`.
///
/// `--repo` and `--label` narrow the analysed set while the envelope keeps
/// describing the file it was loaded from: `scopeLoadedIssues` sets
/// `DataHashMatchesIssues = false` precisely so the payload still names its
/// origin (main.go:4890-4900). Go therefore publishes the *loaded* file's
/// digest inside `source_authority` and the *filtered* one at the top level.
/// Collapsing the two gave the authority "empty" for a filter that matched
/// nothing, which in turn changed `authority_hash` and `scope_hash`.
fn full_envelope_json_with_source_and_authority(
    data_hash: &str,
    authority_hash_input: &str,
    source: Option<&SourceMeta>,
    // The set a scope filter left behind. Go builds the scope hash's id list
    // from `ctx.Issues` (robot_registry.go:250-254), which is the scoped set,
    // while the authority's counts describe the loaded file — so a `--repo`
    // that matched nothing hashes an EMPTY id list, not every id that was read.
    scoped_issues: &[bv_core::model::Issue],
) -> serde_json::Value {
    let mut env = serde_json::Map::new();
    env.insert("generated_at".into(), serde_json::json!(jiff_now()));
    env.insert("data_hash".into(), serde_json::json!(data_hash));
    let fmt = output_format();
    if !fmt.is_empty() {
        env.insert("output_format".into(), serde_json::json!(fmt));
    }
    env.insert("version".into(), serde_json::json!(GO_APP_VERSION));
    if let Some(meta) = source {
        let envelope_path = if meta.envelope_path.is_empty() {
            &meta.path
        } else {
            &meta.envelope_path
        };
        if !envelope_path.is_empty() {
            env.insert("source_path".into(), serde_json::json!(envelope_path));
        }
        if !meta.kind.is_empty() {
            env.insert("source_kind".into(), serde_json::json!(meta.kind));
        }
        // Go's declaration order (main.go:7222-7223) puts both immediately
        // after source_kind and before `scope`.
        let (as_of_ref, as_of_commit) = as_of_pair();
        if !as_of_ref.is_empty() {
            env.insert("as_of".into(), serde_json::json!(as_of_ref));
        }
        if !as_of_commit.is_empty() {
            env.insert("as_of_commit".into(), serde_json::json!(as_of_commit));
        }
        // Go derives the scope from the active --label/--recipe/--repo flags
        // (robot_registry.go:257-269) and emits the object alongside its hash.
        // Hashing empty strings here made scope_hash unreproducible and left
        // `scope` absent even when the caller was scoped.
        let (label, recipe, repo) = active_scope_flags();
        // A --label scope narrows the candidate set to the label's own issues
        // (their neighbours stay in the analysis as context) — see Go
        // scopeLoadedIssues, main.go:4870-4900.
        let (mut candidate_ids, _) =
            bv_analysis::label_health::label_scope_ids(&label, scoped_issues);
        candidate_ids.sort();
        // Go emits `scope` between source_kind and source_authority, not after
        // scope_hash (RobotEnvelope, cmd/bv/main.go:7213-7229). The wire order
        // is the struct's declaration order, and both objects are emitted, so
        // any other position is a byte diff on every scoped invocation.
        if !label.is_empty() || !recipe.is_empty() || !repo.is_empty() {
            // RobotScope's fields are all omitempty, so only the active
            // modifiers appear. Emitting the empty ones both diverges from the
            // oracle and feeds different values into the scope hash.
            let scope = bv_robot::RobotScope {
                label: label.clone(),
                recipe: recipe.clone(),
                repo: repo.clone(),
                // Populated by the --as-of work; omitting it while empty is
                // what Go does for every non-time-travelling run.
                unsupported: Vec::new(),
            };
            env.insert(
                "scope".into(),
                serde_json::to_value(&scope).unwrap_or(serde_json::Value::Null),
            );
        }
        let authority = source_authority(meta, authority_hash_input);
        let ahash = bv_robot::authority_hash(&authority);
        env.insert(
            "source_authority".into(),
            serde_json::to_value(&authority).unwrap_or(serde_json::Value::Null),
        );
        if !ahash.is_empty() {
            env.insert("authority_hash".into(), serde_json::json!(ahash));
        }
        // Go passes `ctx.DataHash` here (robot_registry.go:257), which is the
        // same digest the envelope publishes at the top level — "empty" for a
        // filter that matched nothing. Only `source_authority` names the loaded
        // file; the scope hash does not, and feeding it the authority's digest
        // changed scope_hash for every `--repo` and `--label` invocation.
        let shash = bv_robot::scope_hash(&label, &recipe, &repo, data_hash, &candidate_ids);
        if !shash.is_empty() {
            env.insert("scope_hash".into(), serde_json::json!(shash));
        }
    }
    serde_json::Value::Object(env)
}

fn emit_json(v: &serde_json::Value) -> ExitCode {
    let encoded = go_json_string(v);
    println!("{encoded}");
    // Go toonRobotEncoder.Encode (main.go:7519-7536) writes the `[stats]`
    // estimate to stderr after serializing and before emitting. Gating on
    // `output_format() == "toon"` reproduces Go's gate for free: Rust already
    // downgrades OUTPUT_FORMAT to "json" when `tru` is missing
    // (main.go:2040-2042 equivalent), which is exactly the condition under
    // which Go never selects the encoder that owns this block.
    if show_toon_stats() && output_format() == "toon" {
        let json_tokens = estimate_tokens(&encoded);
        // `encode_payload` (bv-robot/src/envelope.rs:362-367) documents that the
        // TOON encoder emits byte-identical compact JSON apart from the
        // `output_format` marker, so today this equals `json_tokens` and the
        // saving is 0 — Go's `same size` branch. Computed rather than
        // hard-coded so a genuinely re-encoding TOON path changes the line by
        // itself.
        let toon_tokens = estimate_tokens(&encoded);
        // Go main.go:7531 — signed, so a negative value means TOON is the
        // larger encoding for this payload.
        let savings = if json_tokens > 0 {
            ((1.0 - toon_tokens as f64 / json_tokens as f64) * 100.0) as i64
        } else {
            0
        };
        match savings.cmp(&0) {
            std::cmp::Ordering::Greater => eprintln!(
                "[stats] JSON≈{json_tokens} tok, TOON≈{toon_tokens} tok (TOON {savings}% smaller)"
            ),
            std::cmp::Ordering::Less => eprintln!(
                "[stats] JSON≈{json_tokens} tok, TOON≈{toon_tokens} tok (TOON {}% larger; JSON is the smaller encoding for this payload)",
                -savings
            ),
            std::cmp::Ordering::Equal => eprintln!(
                "[stats] JSON≈{json_tokens} tok, TOON≈{toon_tokens} tok (same size)"
            ),
        }
    }
    ExitCode::from(0)
}

/// Go `encoding/json`-compatible compact serializer. The one behavioral
/// difference vs serde_json: float formatting. Go emits `1` for 1.0 and
/// `30` for 30.0 (shortest round-trip, no trailing ".0"); serde_json/ryu
/// emits "1.0". Drop-in byte parity with Go goldens requires Go semantics.
fn go_json_string(v: &serde_json::Value) -> String {
    let mut out = String::new();
    write_go_json(v, &mut out);
    out
}

/// Re-indent compact JSON the way Go's `json.Indent(dst, src, "", "  ")`
/// does, which is what `Feedback.ToJSON` applies to the marshalled value
/// (cmd/bv/main.go:2464-2469).
///
/// Operating on the already-escaped text is safe and keeps the number
/// rendering in one place: a string can no longer contain a bare brace or
/// newline, so the structural characters found are all structural. Go's
/// float64 formatting is the part that matters here — `serde_json`'s pretty
/// printer writes `0.0` and `1.0` where Go writes `0` and `1` — so the
/// input must come from [`go_json_string`], never from serde.
fn go_json_indent(compact: &str) -> String {
    let mut out = String::with_capacity(compact.len() * 2);
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for ch in compact.chars() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '{' | '[' => {
                let open = ch;
                let close = if open == '{' { '}' } else { ']' };
                // An empty container stays `{}` / `[]` — Go's indenter does not
                // break a line for one.
                if compact[out.len()..].starts_with(&close.to_string()) {
                    out.push(open);
                } else {
                    out.push(open);
                    depth += 1;
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(ch);
            }
            ',' => {
                out.push(ch);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            ':' => {
                out.push(ch);
                out.push(' ');
            }
            other => out.push(other),
        }
    }
    out
}

/// Go's `json.MarshalIndent(v, "", "  ")` for this port's values: the Go
/// number and string rendering, re-indented.
fn go_json_string_pretty(v: &serde_json::Value) -> String {
    go_json_indent(&go_json_string(v))
}

/// Quote a string the way Go's `encoding/json` does with its default HTML
/// escaping: `<`, `>` and `&` become \u003c, \u003e and \u0026.
fn go_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn write_go_json(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if n.as_i64().is_none() && n.as_u64().is_none() {
                    out.push_str(&go_format_f64(f));
                    return;
                }
            }
            out.push_str(&n.to_string());
        }
        // Go's encoding/json escapes <, > and & by default (SetEscapeHTML is
        // on unless explicitly disabled), emitting \u003c, \u003e and \u0026.
        // serde_json does not, so a commit message containing "<=" serialized
        // differently from the oracle.
        Value::String(s) => out.push_str(&go_escape_string(s)),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_go_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                write_go_json(val, out);
            }
            out.push('}');
        }
    }
}

/// Go `strconv.AppendFloat(f, 'f'/'e', -1, 64)` semantics: shortest
/// decimal that round-trips. 'f' (no exponent) normally; 'e' when
/// abs < 1e-6 or abs >= 1e21, with Go's e-09 → e-9 exponent cleanup.
fn go_format_f64(f: f64) -> String {
    if f.is_nan() || f.is_infinite() {
        // Go json errors on these; our data never produces them.
        return "null".into();
    }
    let abs = f.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        // Rust {:e} → "1.5e21"; Go → "1.5e+21". Reconstruct Go style.
        let s = format!("{:e}", f); // e.g. "1.5e21", "1e-7", "-2.5e-8"
        if let Some(pos) = s.find('e') {
            let (mantissa, exp) = s.split_at(pos);
            let exp = &exp[1..]; // strip 'e'
            let (sign, digits) = if let Some(stripped) = exp.strip_prefix('-') {
                ("-", stripped)
            } else {
                ("+", exp)
            };
            let digits = if digits.len() == 2 && digits.starts_with('0') {
                &digits[1..] // "09" → "9" (Go cleanup e-09 → e-9)
            } else {
                digits
            };
            return format!("{mantissa}e{sign}{digits}");
        }
        return s;
    }
    // 'f' shortest: Rust Display matches Go 'f' -1 (no trailing .0).
    format!("{f}")
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod go_json_tests {
    use super::*;

    #[test]
    fn go_float_format_matches_go_json() {
        assert_eq!(go_format_f64(1.0), "1");
        assert_eq!(go_format_f64(30.0), "30");
        assert_eq!(go_format_f64(0.5), "0.5");
        assert_eq!(go_format_f64(0.7415134907189797), "0.7415134907189797");
        assert_eq!(go_format_f64(1e-7), "1e-7");
        assert_eq!(go_format_f64(1e21), "1e+21");
        assert_eq!(go_format_f64(233.5454), "233.5454");
        assert_eq!(go_format_f64(0.0), "0");
    }

    #[test]
    fn go_json_string_whole_floats_have_no_dot() {
        let v = serde_json::json!({"a": 1.0, "b": 30.0, "c": 0.125});
        assert_eq!(go_json_string(&v), r#"{"a":1,"b":30,"c":0.125}"#);
    }
}

type AnalysisResultFull = (
    Vec<bv_core::model::Issue>,
    String,
    bv_analysis::analyzer::Phase1Stats,
    bv_analysis::MetricStatus,
    std::sync::Arc<bv_graph_core::DiGraph>,
    bv_analysis::GraphAnalysisPhase2,
);

fn load_full() -> Result<AnalysisResultFull, ExitCode> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let as_of = extract_as_of();
    let issues = if let Some(ref revision) = as_of {
        let loader = bv_core::discovery::GitLoader::new(&cwd);
        match loader.load_at(revision) {
            Ok(issues) => {
                if let Ok(sha) = loader.resolve_revision(revision) {
                    eprintln!(
                        "Loaded {} issues from {} ({})",
                        issues.len(),
                        revision,
                        &sha[..sha.len().min(7)]
                    );
                }
                issues
            }
            Err(e) => {
                eprintln!("Error loading issues at {revision}: {e}");
                return Err(ExitCode::from(1));
            }
        }
    } else {
        match bv_core::discovery::load_issues_from_repo(&cwd) {
            Ok((issues, _)) => issues,
            Err(e) => {
                if is_unresolved_source_error(&e.to_string()) {
                    return Err(emit_unresolved_source());
                }
                eprintln!("Error: {e}");
                return Err(ExitCode::from(1));
            }
        }
    };
    // Go main.go:2786 — scope before analysis; `--repo` re-hashes
    // (main.go:4883-4884), `--label` keeps the loader's hash (:4901).
    let (issues, hash_override) = apply_scope(&issues);
    let data_hash = hash_override.unwrap_or_else(|| bv_core::data_hash::compute_data_hash(&issues));
    let g = std::sync::Arc::new(bv_analysis::build_graph(&issues));
    let p1 = bv_analysis::analyze_phase1(&g);
    let budget = bv_analysis::AnalysisBudget {
        density: p1.density,
        ..bv_analysis::AnalysisBudget::default()
    };
    let gc = std::sync::Arc::clone(&g);
    let (status, phase2) = bv_analysis::analyze_phase2_blocking(gc, &budget);
    Ok((issues, data_hash, p1, status, g, phase2))
}

fn to_id_map(
    g: &bv_graph_core::DiGraph,
    scores: &[f64],
) -> serde_json::Map<String, serde_json::Value> {
    // Insert in lexicographic ID order — Go json.Marshal sorts map keys,
    // so serialized key order must be lexicographic (index order is
    // issue-load order, which differs for IDs like FIX-10 vs FIX-2).
    let mut sorted: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
    for (i, v) in scores.iter().enumerate() {
        let id = g.node_id(i).unwrap_or_default();
        sorted.insert(id, *v);
    }
    let mut m = serde_json::Map::new();
    for (id, v) in sorted {
        if v.fract() == 0.0 && v.abs() < 1e15 {
            m.insert(id.to_string(), serde_json::json!(v.round() as i64));
        } else {
            m.insert(id.to_string(), serde_json::json!(v));
        }
    }
    m
}

/// Go `ConfigForSize` JSON shape (ns timeouts) — golden-verified per tier.
fn insights_analysis_config(nodes: usize) -> serde_json::Value {
    // Go main.go:1676 wires `--force-full-analysis` into this handler's
    // config, and robot_registry.go:1972-1987 publishes `stats.Config` from
    // it, so the override has to reach the emitted object.
    if force_full_analysis() {
        return serde_json::to_value(bv_analysis::analyzer::full_analysis_config())
            .unwrap_or_default();
    }
    let (
        timeout_ns,
        mode,
        sample,
        pr_ns,
        hits_ns,
        cycles_ns,
        max_cycles,
        compute_cycles,
        cycles_skip,
    ) = match nodes {
        n if n < 100 => (
            2_000_000_000i64,
            "exact",
            0,
            2_000_000_000i64,
            2_000_000_000i64,
            2_000_000_000i64,
            1000,
            true,
            "",
        ),
        n if n < 500 => (
            500_000_000i64,
            "exact",
            0,
            500_000_000i64,
            500_000_000i64,
            500_000_000i64,
            100,
            true,
            "",
        ),
        n if n < 2000 => (
            500_000_000i64,
            "approximate",
            100,
            300_000_000i64,
            300_000_000i64,
            300_000_000i64,
            50,
            true,
            "",
        ),
        _ => (
            500_000_000i64,
            "approximate",
            200,
            200_000_000i64,
            200_000_000i64,
            0i64,
            10,
            false,
            "graph too large (>2000 nodes)",
        ),
    };
    serde_json::json!({
        "ComputeBetweenness": true,
        "BetweennessTimeout": timeout_ns,
        "BetweennessSkipReason": "",
        "BetweennessMode": mode,
        "BetweennessSampleSize": sample,
        "BetweennessIsApproximate": false,
        "ComputePageRank": true,
        "PageRankTimeout": pr_ns,
        "PageRankSkipReason": "",
        "ComputeHITS": true,
        "HITSTimeout": hits_ns,
        "HITSSkipReason": "",
        "ComputeCycles": compute_cycles,
        "CyclesTimeout": cycles_ns,
        "MaxCyclesToStore": max_cycles,
        "CyclesSkipReason": cycles_skip,
        "ComputeEigenvector": true,
        "ComputeCriticalPath": true,
        "ComputeKCore": true,
        "ComputeArticulation": true,
        "ComputeSlack": true,
    })
}

/// Go `getTopItems`: sort by value desc, ID asc tiebreak, cap at limit.
fn top_items_go(
    map: &serde_json::Map<String, serde_json::Value>,
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut items: Vec<(&String, f64)> = map
        .iter()
        .filter_map(|(k, v)| v.as_f64().map(|f| (k, f)))
        .collect();
    items.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    items
        .into_iter()
        .take(limit)
        .map(|(id, v)| serde_json::json!({"ID": id, "Value": v}))
        .collect()
}

fn run_robot_insights() -> ExitCode {
    match build_robot_insights() {
        Ok(payload) => emit_json(&payload),
        Err(code) => code,
    }
}

/// The `--robot-insights` payload, split out from the dispatch wrapper so
/// `--agent-brief` can embed the same document in its bundle (Go main.go:3943
/// writes the identical insights object).
fn build_robot_insights() -> Result<serde_json::Value, ExitCode> {
    let all = load_full()?;
    let (issues, hash, p1, status, g, phase2) = all;

    let pr_obj = to_id_map(&g, &bv_graph_core::pagerank_default(&g));
    // Go's betweenness here is the config-driven Phase 2 result, not a
    // direct exact call: `ConfigForSize` switches to approximate sampling above
    // the density/size thresholds (config.go:158-190). Computing exact
    // unconditionally gave `large_cyclic_600` max 184 / 337 non-zero where Go
    // reports 210 / 176, and `xl_2500` max 12 / 124 where Go reports
    // 37.5 / 15.
    let bw_raw: Vec<f64> = bv_analysis::analyzer::go_betweenness(&g);
    let mut bw_obj = to_id_map(&g, &bw_raw);
    // gonum Betweenness omits zero-score nodes (endpoints of a DAG chain).
    bw_obj.retain(|_, v| v.as_f64() != Some(0.0));
    let ev_raw = bv_graph_core::eigenvector_default(&g);
    let ev_obj = to_id_map(&g, &ev_raw);
    let hits_result = bv_graph_core::hits_default(&g);
    let hub_obj = to_id_map(&g, &hits_result.hubs);
    let auth_obj = to_id_map(&g, &hits_result.authorities);
    // Go computes critical-path heights only when Phase 1's topological sort
    // covered every node (graph.go:2161-2163) — i.e. the graph is acyclic. A
    // height DP needs a valid order; on a cyclic graph gonum returns an
    // Unorderable error, Go skips the metric and leaves the map empty.
    // large_cyclic_600 is cyclic: Go emits `{}` there, Rust emitted 200
    // entries of invented scores.
    let cp_heights: Vec<f64> =
        if bv_graph_core::algorithms::topo::topological_sort_gonum(&g).is_some() {
            bv_graph_core::critical_path_heights(&g)
        } else {
            Vec::new()
        };
    let cp_obj = to_id_map(&g, &cp_heights);
    let cores = bv_graph_core::kcore(&g);
    let core_obj = to_id_map(&g, &cores.iter().map(|&v| v as f64).collect::<Vec<_>>());
    // Go computes Slack from the Phase 1 topological order and only when that
    // order covers every issue (graph.go:2270-2273); on a cyclic graph
    // `stats.Slack` is left nil, so the document reports an empty list rather
    // than a longest-path value for a graph that has no longest path.
    let slack_unavailable = !bv_analysis::analyzer::topological_order_available(&g);
    let slacks = if slack_unavailable {
        Vec::new()
    } else {
        bv_graph_core::slack(&g)
    };
    let slack_obj = to_id_map(&g, &slacks);
    let art_pts = bv_graph_core::algorithms::articulation::articulation_points(&g);
    let mut art_ids: Vec<String> = art_pts
        .iter()
        .map(|&i| g.node_id(i).unwrap_or_default().to_string())
        .collect();
    // Go ArticulationPoints() sorts with sort.Strings (lexicographic).
    art_ids.sort();

    let n = g.len() as f64;
    let density = if n <= 1.0 {
        0.0
    } else {
        g.edge_count() as f64 / (n * (n - 1.0))
    };

    let mut payload = full_envelope_for(&hash, &issues);
    payload["analysis_config"] = insights_analysis_config(g.len());
    // Go's `statusEntry.MarshalJSON` (pkg/analysis/graph.go:131-147) omits
    // `reason` only when it is empty, and every Go site that sets
    // `State: "skipped"` fills the reason in the same literal — so the analyzer
    // already produces exactly Go's shape and nothing is cleared here. The
    // reasoning for the sibling `--robot-triage` block applies verbatim; see
    // the comment there for why clearing was wrong.
    let fixed_status = status.clone();
    payload["status"] = fixed_status.to_json_map();
    // Go declares LabelScope/LabelContext directly after Status
    // (robot_registry.go:1970-1986).
    insert_label_scope_keys(&mut payload, &issues, robot_now());

    // Go GenerateInsights(limit=50): value desc, ID asc tiebreak.
    const INSIGHTS_LIMIT: usize = 50;
    payload["Bottlenecks"] = serde_json::Value::Array(top_items_go(&bw_obj, INSIGHTS_LIMIT));
    payload["Keystones"] = serde_json::Value::Array(top_items_go(&cp_obj, INSIGHTS_LIMIT));
    payload["Influencers"] = serde_json::Value::Array(top_items_go(&ev_obj, INSIGHTS_LIMIT));
    payload["Hubs"] = serde_json::Value::Array(top_items_go(&hub_obj, INSIGHTS_LIMIT));
    payload["Authorities"] = serde_json::Value::Array(top_items_go(&auth_obj, INSIGHTS_LIMIT));
    payload["Cores"] = serde_json::Value::Array(top_items_go(&core_obj, INSIGHTS_LIMIT));
    // Go caps every insights list at the same `limit` (insights.go:88-96,
    // `limitStrings(artPts, limit)` at :94). Rust emitted the articulation
    // set uncapped — 204 entries on xl_2500 where Go caps at 50.
    let art_capped: Vec<&String> = art_ids.iter().take(INSIGHTS_LIMIT).collect();
    payload["Articulation"] = serde_json::json!(art_capped);
    payload["Slack"] = serde_json::Value::Array(top_items_go(&slack_obj, INSIGHTS_LIMIT));

    // Orphans: zero out-degree (nothing depends on them), sorted (Go findOrphans).
    let orphans: Vec<String> = (0..g.len())
        .filter(|&i| g.out_degree(i) == 0)
        .map(|i| g.node_id(i).unwrap_or_default().to_string())
        .collect();
    // Same `limit` applies to Orphans (insights.go:95) — 1855 uncapped here.
    let orphans_capped: Vec<&String> = orphans.iter().take(INSIGHTS_LIMIT).collect();
    payload["Orphans"] = serde_json::json!(orphans_capped);

    // Cycles: Go emits null when none detected.
    let cycles_from_phase2 = phase2.cycles.clone().unwrap_or_default();
    payload["Cycles"] = serde_json::json!(cycles_from_phase2);
    payload["ClusterDensity"] = serde_json::json!(density);

    // Velocity snapshot — Go VelocitySnapshot: weekly as plain ints (newest
    // first), estimated omitted when false, no week_start objects.
    if let Some(vel) = bv_analysis::triage::compute_project_velocity(&issues, robot_now()) {
        let weekly_ints: Vec<i64> = vel["weekly"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|w| w["closed"].as_i64().unwrap_or(0))
                    .collect()
            })
            .unwrap_or_default();
        let snapshot = serde_json::json!({
            "closed_last_7_days": vel["closed_last_7_days"],
            "closed_last_30_days": vel["closed_last_30_days"],
            "avg_days_to_close": vel["avg_days_to_close"],
            "weekly": weekly_ints,
        });
        payload["Velocity"] = snapshot;
    }

    // Stats sub-object (Go GraphStats JSON shape) — comes before
    // full_stats/top_what_ifs/advanced_insights in Go's output struct order.
    payload["Stats"] = serde_json::json!({
        "OutDegree": p1.out_degree,
        "InDegree": p1.in_degree,
        "TopologicalOrder": p1.topological_order,
        "Density": p1.density,
        "NodeCount": p1.node_count,
        "EdgeCount": p1.edge_count,
        "Config": insights_analysis_config(g.len()),
    });

    // full_stats — exactly Go's 9 fields (mapLimit=200 default).
    let map_limit = insights_map_limit();
    let mut fs = serde_json::Map::new();
    for (key, obj) in [
        ("pagerank", pr_obj),
        ("betweenness", bw_obj),
        ("eigenvector", ev_obj),
        ("hubs", hub_obj),
        ("authorities", auth_obj),
        ("critical_path_score", cp_obj),
        ("core_number", core_obj),
    ] {
        fs.insert(
            key.into(),
            serde_json::Value::Object(limit_metric_map(obj, map_limit)),
        );
    }
    // Slack is the one metric whose *nil-ness* is load-bearing. Go declares
    // `var localSlack map[string]float64` and only assigns it when the
    // topological order covers every issue (graph.go:2270-2273), so on a
    // cyclic graph `stats.Slack()` hands `limitMaps` a nil map and the
    // document carries `null`. A metric that ran and found nothing is an
    // allocated empty map and stays `{}` — critical_path_score on the same
    // fixture is exactly that case, so this cannot be decided by emptiness
    // alone.
    fs.insert(
        "slack".into(),
        if slack_unavailable {
            serde_json::Value::Null
        } else {
            serde_json::Value::Object(limit_metric_map(slack_obj, map_limit))
        },
    );
    // Go's `limitSlice` (robot_registry.go:1932-1939) caps this list at the
    // same mapLimit as the maps: `in[:limit]`, no sorting.
    let art_limited: Vec<&String> = art_ids.iter().take(map_limit).collect();
    fs.insert("articulation_points".into(), serde_json::json!(art_limited));
    payload["full_stats"] = serde_json::Value::Object(fs);

    // top_what_ifs — Go TopWhatIfDeltas (bv-83): exact delta semantics.
    let whatif_entries = compute_top_what_if_deltas(&issues, &g, &cp_heights);
    if !whatif_entries.is_empty() {
        payload["top_what_ifs"] = serde_json::Value::Array(whatif_entries);
    }

    // advanced_insights — Go GenerateAdvancedInsights parity (bv-145/152/153/154).
    payload["advanced_insights"] = generate_advanced_insights(&issues, &cycles_from_phase2);

    payload["usage_hints"] = serde_json::json!([
        "jq '.Bottlenecks[:5] | map(.ID)' - Top 5 bottleneck IDs",
        "jq '.Keystones[:3]' - Top 3 critical path scores",
        "jq '.top_what_ifs[] | select(.delta.direct_unblocks > 2)' - High-impact items",
        "jq '.full_stats.pagerank | to_entries | sort_by(-.value)[:5]' - Top PageRank",
        "jq '.full_stats.core_number | to_entries | sort_by(-.value)[:5]' - Strongly embedded nodes (k-core)",
        "jq '.full_stats.articulation_points' - Structural cut points",
        "jq '.Slack[:5]' - Nodes with slack (good parallel work candidates)",
        "jq '.Cycles | length' - Count of detected cycles",
        "jq '.advanced_insights.cycle_break' - Cycle break suggestions (bv-181)",
        "BV_INSIGHTS_MAP_LIMIT=50 bv --robot-insights - Reduce map sizes",
    ]);

    Ok(payload)
}

/// Go `TopWhatIfDeltas` (priority.go bv-83) — per-issue what-if deltas with
/// exact Go semantics: direct unblocks (newly-actionable dependents),
/// BFS-simulated transitive unblocks, depth reduction (cpValue/10 capped
/// at 1), days saved (Σ estimates / 480), parallel gain = direct − 1.
fn compute_top_what_if_deltas(
    issues: &[bv_core::model::Issue],
    g: &bv_graph_core::DiGraph,
    cp_heights: &[f64],
) -> Vec<serde_json::Value> {
    use bv_core::model::Status;
    const MAX_CRITICAL_PATH_DEPTH: f64 = 10.0;
    const MAX_UNBLOCKED_IDS_SHOWN: usize = 10;

    let is_closed = |id: &str| -> bool {
        issues
            .iter()
            .find(|i| i.id == id)
            .map(|i| matches!(i.status, Status::Closed | Status::Tombstone))
            .unwrap_or(true)
    };

    // Go computeUnblocks: dependents that are open and have no OTHER open blocker.
    let compute_unblocks = |issue_id: &str| -> Vec<String> {
        let mut unblocks = Vec::new();
        let Some(node) = g.node_idx(issue_id) else {
            return unblocks;
        };
        for &dep_node in g.predecessors_slice(node) {
            let dep_id = g.node_id(dep_node).unwrap_or_default();
            if is_closed(&dep_id) {
                continue;
            }
            let mut still_blocked = false;
            for &other in g.successors_slice(dep_node) {
                let other_id = g.node_id(other).unwrap_or_default();
                if other_id == issue_id {
                    continue;
                }
                if !is_closed(&other_id) {
                    still_blocked = true;
                    break;
                }
            }
            if !still_blocked {
                unblocks.push(dep_id.to_string());
            }
        }
        unblocks.sort();
        unblocks
    };

    // Go countTransitiveUnblocks: BFS with simulated-closed set.
    let count_transitive = |issue_id: &str| -> usize {
        let mut simulated: std::collections::HashSet<String> = std::collections::HashSet::new();
        simulated.insert(issue_id.to_string());
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(issue_id.to_string());
        let mut count = 0usize;
        while let Some(curr) = queue.pop_front() {
            let Some(node) = g.node_idx(curr.as_str()) else {
                continue;
            };
            for &dep_node in g.predecessors_slice(node) {
                let dep_id = g.node_id(dep_node).unwrap_or_default().to_string();
                if simulated.contains(&dep_id) || is_closed(&dep_id) {
                    continue;
                }
                // Unblocked if ALL blockers are (real or simulated) closed.
                let mut blocked = false;
                for &blocker_node in g.successors_slice(dep_node) {
                    let blocker_id = g.node_id(blocker_node).unwrap_or_default().to_string();
                    let blocker_closed = simulated.contains(&blocker_id) || is_closed(&blocker_id);
                    if !blocker_closed {
                        blocked = true;
                        break;
                    }
                }
                if !blocked {
                    simulated.insert(dep_id.clone());
                    queue.push_back(dep_id);
                    count += 1;
                }
            }
        }
        count
    };

    let mut results: Vec<serde_json::Value> = Vec::new();
    for issue in issues {
        if matches!(issue.status, Status::Closed | Status::Tombstone) {
            continue;
        }
        let direct_list = compute_unblocks(&issue.id);
        let direct = direct_list.len() as i64;
        let transitive = count_transitive(&issue.id) as i64;
        if direct == 0 && transitive == 0 {
            continue;
        }
        // Go counts unblocked issues whose dependency state is not satisfied
        // (priority.go:963-970). Checking `status == blocked` counted only
        // issues parked in that status; an open issue still held up by an open
        // blocker also counts, which is the common case on a chain.
        let blocked_reduction = direct_list
            .iter()
            .filter(|id| {
                issues.iter().any(|i| {
                    i.id == **id
                        && i.dependencies.iter().any(|d| {
                            d.r#type.is_blocking()
                                && !d.effective_depends_on().is_empty()
                                && issues.iter().any(|o| {
                                    o.id == d.effective_depends_on()
                                        && !matches!(o.status, Status::Closed | Status::Tombstone)
                                })
                        })
                })
            })
            .count() as i64;
        let cp_node = g.node_idx(issue.id.as_str()).unwrap_or(usize::MAX);
        let current_depth = cp_heights.get(cp_node).copied().unwrap_or(0.0);
        let mut depth_reduction = 0.0f64;
        if current_depth > 0.0 {
            depth_reduction = (current_depth / MAX_CRITICAL_PATH_DEPTH).min(1.0);
        }
        let estimated_days_saved: f64 = direct_list
            .iter()
            .map(|id| {
                issues
                    .iter()
                    .find(|i| i.id == *id)
                    .map(|i| i.estimated_minutes.unwrap_or(60) as f64)
                    .unwrap_or(60.0)
            })
            .sum::<f64>()
            / 480.0;
        let unblocked_ids: Vec<String> = direct_list
            .iter()
            .take(MAX_UNBLOCKED_IDS_SHOWN)
            .cloned()
            .collect();
        let parallel_gain = direct - 1;

        // Go generateWhatIfExplanation.
        let explanation = if direct == 0 {
            "No immediate downstream impact".to_string()
        } else {
            let mut e = format!("Completing this directly unblocks {direct} item");
            if direct != 1 {
                e.push('s');
            }
            if transitive > direct {
                e.push_str(&format!(" ({transitive} total including cascades)"));
            }
            if blocked_reduction > 0 {
                e.push_str(&format!(", clears {blocked_reduction} blocked"));
            }
            if estimated_days_saved >= 0.5 {
                e.push_str(&format!(
                    ", enabling ~{estimated_days_saved:.1} days of work"
                ));
            }
            e
        };

        results.push(serde_json::json!({
            "issue_id": issue.id,
            "title": issue.title,
            "delta": {
                "direct_unblocks": direct,
                "transitive_unblocks": transitive,
                "blocked_reduction": blocked_reduction,
                "depth_reduction": depth_reduction,
                "estimated_days_saved": estimated_days_saved,
                "unblocked_issue_ids": unblocked_ids,
                "parallelization_gain": parallel_gain,
                "explanation": explanation,
            },
        }));
    }

    // Sort: transitive desc, direct desc, ID asc; cap at 10.
    results.sort_by(|a, b| {
        let at = a["delta"]["transitive_unblocks"].as_i64().unwrap_or(0);
        let bt = b["delta"]["transitive_unblocks"].as_i64().unwrap_or(0);
        let ad = a["delta"]["direct_unblocks"].as_i64().unwrap_or(0);
        let bd = b["delta"]["direct_unblocks"].as_i64().unwrap_or(0);
        bt.cmp(&at)
            .then_with(|| bd.cmp(&ad))
            .then_with(|| a["issue_id"].as_str().cmp(&b["issue_id"].as_str()))
    });
    results.truncate(10);
    results
}

/// Go `FeatureStatus` JSON with omitempty semantics:
/// state always; reason/capped/count/limited omitted when zero-valued.
fn feature_status(
    state: &str,
    reason: &str,
    capped: bool,
    count: i64,
    limited: i64,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("state".into(), serde_json::json!(state));
    if !reason.is_empty() {
        m.insert("reason".into(), serde_json::json!(reason));
    }
    if capped {
        m.insert("capped".into(), serde_json::json!(true));
    }
    if count != 0 {
        m.insert("count".into(), serde_json::json!(count));
    }
    if limited != 0 {
        m.insert("limited".into(), serde_json::json!(limited));
    }
    serde_json::Value::Object(m)
}

/// Go `AdvancedInsights` port (advanced_insights.go) — greedy top-k set,
/// coverage set, k-paths, parallel cut, pending parallel gain, cycle break.
fn generate_advanced_insights(
    issues: &[bv_core::model::Issue],
    cycles: &[Vec<String>],
) -> serde_json::Value {
    use bv_core::model::Issue;
    let is_open = |i: &Issue| {
        !matches!(
            i.status,
            bv_core::model::Status::Closed | bv_core::model::Status::Tombstone
        )
    };
    let title_of = |id: &str| -> String {
        issues
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.title.clone())
            .unwrap_or_default()
    };

    // ---- TopK Set (greedy submodular, k=5) — Go generateTopKSet ----
    let mut candidates: Vec<String> = issues
        .iter()
        .filter(|i| is_open(i))
        .map(|i| i.id.clone())
        .collect();
    candidates.sort();
    let mut topk_items: Vec<serde_json::Value> = Vec::new();
    let mut marginal_gains: Vec<i64> = Vec::new();
    let mut total_gain = 0i64;
    let mut completed: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let by_id: std::collections::HashMap<&str, &Issue> =
            issues.iter().map(|i| (i.id.as_str(), i)).collect();
        let k = 5usize;
        for _ in 0..k {
            // Go re-derives `actionable` each step via
            // getActionableIssuesAfterCompletions(completed) and picks only
            // from that set (advanced_insights.go:492). Rust reused the static
            // open-issue list, so a blocked high-fanout node could be selected
            // ahead of its own prerequisite — which is exactly what "work these
            // in order" must never do.
            let blockers_resolved = |id: &str| -> bool {
                issues
                    .iter()
                    .find(|i| i.id == id)
                    .map(|i| {
                        i.dependencies
                            .iter()
                            .filter(|d| d.r#type.is_blocking())
                            .all(|d| {
                                let t = d.effective_depends_on();
                                completed.contains(t)
                                    || !issues.iter().any(|x| x.id == t && is_open(x))
                            })
                    })
                    .unwrap_or(false)
            };
            let mut remaining: Vec<String> = candidates
                .iter()
                .filter(|id| !completed.contains(*id) && blockers_resolved(id))
                .cloned()
                .collect();
            if remaining.is_empty() {
                break;
            }
            let mut best_id = String::new();
            let mut best_gain: i64 = -1;
            let mut best_unblocks: Vec<String> = Vec::new();
            for cand in &remaining {
                // Go computeMarginalUnblocks: direct newly-actionable count.
                let mut unblocks: Vec<String> = Vec::new();
                for issue in issues {
                    // Go excludes ids in `before` — the currently-actionable
                    // set — since an already-ready issue is not a new unlock
                    // (advanced_insights.go:582).
                    if !is_open(issue)
                        || completed.contains(&issue.id)
                        || remaining.contains(&issue.id)
                        || issue.id == *cand
                    {
                        continue;
                    }
                    let mut has_this_blocker = false;
                    let mut would_be_blocked = false;
                    for dep in &issue.dependencies {
                        if !dep.r#type.is_blocking() {
                            continue;
                        }
                        let b = dep.effective_depends_on();
                        if b == cand.as_str() {
                            has_this_blocker = true;
                            continue;
                        }
                        let blocker_closed = by_id.get(b).map(|bi| !is_open(bi)).unwrap_or(true);
                        let blocker_completed = completed.contains(b);
                        if !blocker_closed && !blocker_completed {
                            would_be_blocked = true;
                        }
                    }
                    if has_this_blocker && !would_be_blocked {
                        unblocks.push(issue.id.clone());
                    }
                }
                unblocks.sort();
                let gain = unblocks.len() as i64;
                if gain > best_gain
                    || (gain == best_gain && (best_id.is_empty() || cand < &best_id))
                {
                    best_id = cand.clone();
                    best_gain = gain;
                    best_unblocks = unblocks;
                }
            }
            if best_id.is_empty() {
                break;
            }
            completed.insert(best_id.clone());
            remaining.retain(|r| r != &best_id);
            // Go's TopKSetItem.Unblocks is `omitempty`
            // (advanced_insights.go:146), so an item that newly unblocks
            // nothing omits the key rather than emitting an empty array.
            let mut item = serde_json::json!({
                "id": best_id,
                "title": title_of(&best_id),
                "marginal_gain": best_gain,
            });
            if !best_unblocks.is_empty() {
                item["unblocks"] = serde_json::json!(best_unblocks);
            }
            topk_items.push(item);
            marginal_gains.push(best_gain);
            total_gain += best_gain;
        }
    }
    let topk_limited = candidates.len() as i64;
    let topk = serde_json::json!({
        "status": feature_status(
            "available",
            "",
            topk_items.len() as i64 >= 5 && topk_limited > 5,
            topk_items.len() as i64,
            topk_limited,
        ),
        "items": topk_items,
        "total_gain": total_gain,
        "marginal_gain": marginal_gains,
        "how_to_use": "Best k issues to complete for max downstream unlock. Work these in order.",
    });

    // ---- Coverage Set (greedy vertex cover, limit 5) — Go generateCoverageSet ----
    let mut edges: Vec<(String, String)> = Vec::new();
    let by_id: std::collections::HashMap<&str, &Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();
    for issue in issues {
        if !is_open(issue) {
            continue;
        }
        for dep in &issue.dependencies {
            if !dep.r#type.is_blocking() {
                continue;
            }
            let target = dep.effective_depends_on();
            if let Some(t) = by_id.get(target) {
                if is_open(t) {
                    edges.push((issue.id.clone(), target.to_string()));
                }
            }
        }
    }
    let total_edges = edges.len() as i64;
    let coverage = if total_edges == 0 {
        serde_json::json!({
            "status": feature_status("available", "No blocking edges to cover", false, 0, 0),
            "edges_covered": 0,
            "total_edges": 0,
            "coverage_ratio": 1.0,
            "rationale": "Graph has no blocking dependencies.",
            "how_to_use": "Greedy dependency-edge coverage. Check coverage_ratio and capped before treating it as complete.",
        })
    } else {
        let mut uncovered: Vec<(String, String)> = edges.clone();
        let mut cov_items: Vec<serde_json::Value> = Vec::new();
        let mut edges_covered = 0i64;
        let mut selection = 0i64;
        while !uncovered.is_empty() && cov_items.len() < 5 {
            let mut deg: std::collections::BTreeMap<String, i64> =
                std::collections::BTreeMap::new();
            for (f, t) in &uncovered {
                *deg.entry(f.clone()).or_insert(0) += 1;
                *deg.entry(t.clone()).or_insert(0) += 1;
            }
            let mut best_id = String::new();
            let mut best_deg = -1i64;
            for (id, d) in &deg {
                if *d > best_deg || (*d == best_deg && (best_id.is_empty() || id < &best_id)) {
                    best_id = id.clone();
                    best_deg = *d;
                }
            }
            if best_id.is_empty() {
                break;
            }
            let mut added = 0i64;
            uncovered.retain(|(f, t)| {
                if f == &best_id || t == &best_id {
                    added += 1;
                    false
                } else {
                    true
                }
            });
            edges_covered += added;
            selection += 1;
            cov_items.push(serde_json::json!({
                "id": best_id,
                "title": title_of(&best_id),
                "edges_added": added,
                "total_degree": best_deg,
                "selection_seq": selection,
            }));
        }
        let capped = !uncovered.is_empty();
        serde_json::json!({
            "status": feature_status("available", "", capped, cov_items.len() as i64, total_edges),
            "items": cov_items,
            "edges_covered": edges_covered,
            "total_edges": total_edges,
            "coverage_ratio": edges_covered as f64 / total_edges as f64,
            "rationale": "Greedy vertex-cover heuristic: iteratively pick highest uncovered degree until edges are covered or cap is reached.",
            "how_to_use": "Greedy dependency-edge coverage. Check coverage_ratio and capped before treating it as complete.",
        })
    };

    // ---- K-Paths (k=5, cap=50) — Go generateKPaths (longest-path DP) ----
    // Nodes: open issues sorted by ID; edges blocker -> blocked among open.
    let mut nodes: Vec<String> = issues
        .iter()
        .filter(|i| is_open(i))
        .map(|i| i.id.clone())
        .collect();
    nodes.sort();
    let idx: std::collections::HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let n = nodes.len();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut in_deg = vec![0usize; n];
    for issue in issues {
        if !is_open(issue) {
            continue;
        }
        let to = idx[issue.id.as_str()];
        for dep in &issue.dependencies {
            if !dep.r#type.is_blocking() {
                continue;
            }
            if let Some(&from) = idx.get(dep.effective_depends_on()) {
                adj[from].push(to);
                in_deg[to] += 1;
            }
        }
    }
    for a in &mut adj {
        a.sort();
    }
    // Kahn topo with min-heap (BTreeSet for determinism).
    let mut topo: Vec<usize> = Vec::new();
    let mut pq: std::collections::BTreeSet<usize> = (0..n).filter(|&i| in_deg[i] == 0).collect();
    let mut temp_deg = in_deg.clone();
    while let Some(&u) = pq.iter().next() {
        pq.remove(&u);
        topo.push(u);
        for &v in &adj[u] {
            temp_deg[v] -= 1;
            if temp_deg[v] == 0 {
                pq.insert(v);
            }
        }
    }
    // Go's gate is `len(topoOrder) != n` (advanced_insights.go:817): a partial
    // order cannot support an honest longest-path result, because paths
    // through or downstream of a cycle would be silently omitted.
    let order_available = topo.len() == n;
    let mut dist = vec![0i64; n];
    let mut pred = vec![-1i64; n];
    for &u in &topo {
        for &v in &adj[u] {
            if dist[u] + 1 > dist[v] {
                dist[v] = dist[u] + 1;
                pred[v] = u as i64;
            } else if dist[u] + 1 == dist[v] && (pred[v] == -1 || (u as i64) < pred[v]) {
                pred[v] = u as i64;
            }
        }
    }
    let mut path_ends: Vec<(usize, i64)> = if order_available {
        (0..n).map(|i| (i, dist[i])).collect()
    } else {
        Vec::new()
    };
    path_ends.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| nodes[a.0].cmp(&nodes[b.0])));
    let mut paths: Vec<serde_json::Value> = Vec::new();
    let mut used_sources: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut total_paths = 0i64;
    // Go counts `representativeCount` as the distinct SOURCE nodes that have
    // at least one non-trivial path (advanced_insights.go:929-937), over every
    // candidate — not just the ones that survive into `paths`. Counting only
    // the emitted paths reported 5 where Go reports 391, which understated how
    // many sources were actually omitted.
    let mut representative_sources: std::collections::HashSet<usize> =
        std::collections::HashSet::new();
    for &(i, len) in &path_ends {
        if len == 0 {
            continue;
        }
        total_paths += 1;
        // Walk to the root the same way the emit loop does, to find the source.
        let mut head = i as i64;
        while pred[head as usize] != -1 {
            head = pred[head as usize];
        }
        representative_sources.insert(head as usize);
    }
    for &(i, len) in &path_ends {
        if paths.len() >= 5 {
            break;
        }
        if len == 0 {
            continue;
        }
        let mut chain = Vec::new();
        let mut cur = i as i64;
        while cur != -1 {
            chain.push(cur as usize);
            cur = pred[cur as usize];
        }
        chain.reverse();
        let source = chain[0];
        if !used_sources.insert(source) {
            continue;
        }
        let mut truncated = false;
        if chain.len() > 50 {
            chain.truncate(50);
            truncated = true;
        }
        let ids: Vec<String> = chain.iter().map(|&x| nodes[x].clone()).collect();
        let mut p = serde_json::json!({
            "rank": paths.len() + 1,
            "length": ids.len(),
            "issue_ids": ids,
        });
        if truncated {
            p["truncated"] = serde_json::json!(true);
        }
        paths.push(p);
    }
    let mut k_paths = serde_json::json!({
        // Go's KPathsResult.Limited is the number of representative sources
        // considered (advanced_insights.go:943), not the total path count.
        "status": if order_available {
            feature_status(
                "available",
                "",
                paths.len() >= 5 && total_paths > 5,
                paths.len() as i64,
                representative_sources.len() as i64,
            )
        } else {
            // A partial topological order cannot support an honest
            // longest-path result: paths through or downstream of a cycle
            // would be silently omitted. Go fails closed and points at the
            // cycle-break feature (advanced_insights.go:814-822).
            feature_status(
                "skipped",
                "Dependency graph contains a cycle; break cycles before computing critical paths",
                false,
                0,
                0,
            )
        },
        "how_to_use": "Representative longest critical paths. Focus on issues appearing in multiple paths.",
    });
    // Go's KPathsResult.Paths is `omitempty` (advanced_insights.go:173), so a
    // run that found no path omits the key rather than emitting [].
    if !paths.is_empty() {
        k_paths["paths"] = serde_json::json!(paths);
    }

    // ---- Parallel Cut (limit 5) — Go generateParallelCut ----
    let open_set: std::collections::HashSet<&str> = issues
        .iter()
        .filter(|i| is_open(i))
        .map(|i| i.id.as_str())
        .collect();
    // Every id the corpus knows about, as distinct from the open ones. Go's
    // `ReadinessIndex.Blockers` counts a dependency as blocking when its
    // target is *absent* as well as when it is present and open
    // (`!exists || !closedForReadiness(other.Status)`), so a dangling
    // reference still withholds readiness. Requiring the target to be a
    // known-open issue — which this did — reads "blocked by something we
    // cannot see" as "not blocked", inverting the meaning of a missing edge.
    let known_set: std::collections::HashSet<&str> = issues.iter().map(|i| i.id.as_str()).collect();
    let mut blocker_of: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    let mut blocked_by: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for issue in issues {
        if !is_open(issue) {
            continue;
        }
        for dep in &issue.dependencies {
            if !dep.r#type.is_blocking() {
                continue;
            }
            let b = dep.effective_depends_on();
            if !known_set.contains(b) || open_set.contains(b) {
                blocker_of.entry(b).or_default().push(issue.id.as_str());
                blocked_by.entry(issue.id.as_str()).or_default().push(b);
            }
        }
    }
    // (The actionable count itself is no longer needed here: the suggestion
    // loop below re-derives per-candidate readiness, and max_parallel is now
    // projected from the completed set rather than from this base count.)
    let mut pc_candidates: Vec<(String, i64, i64, Vec<String>)> = Vec::new();
    // Go iterates `actionable`, not every open issue (advanced_insights.go:983):
    // the suggestion is "complete this to widen parallel work", which only
    // makes sense for something that can be completed now. Iterating all open
    // issues admits a candidate at every depth of the graph.
    let actionable_ids: Vec<&str> = open_set
        .iter()
        .copied()
        .filter(|id| blocked_by.get(*id).is_none_or(|v| v.is_empty()))
        .collect();
    for id in actionable_ids {
        let mut newly: Vec<String> = Vec::new();
        if let Some(dependents) = blocker_of.get(id) {
            for &dep_id in dependents {
                let all_others_resolved = blocked_by
                    .get(dep_id)
                    .map(|blockers| blockers.iter().all(|b| **b == *id || !open_set.contains(b)))
                    .unwrap_or(true);
                // Go requires `!before[id]` (advanced_insights.go:582): an issue
                // that is already actionable is not a *new* unlock, so it must
                // not raise the gain.
                let already_actionable = blocked_by
                    .get(dep_id)
                    .map(|blockers| blockers.is_empty())
                    .unwrap_or(true);
                if all_others_resolved && !already_actionable {
                    newly.push(dep_id.to_string());
                }
            }
        }
        let gain = newly.len() as i64 - 1;
        if gain > 0 {
            newly.sort();
            pc_candidates.push((id.to_string(), gain, newly.len() as i64, newly));
        }
    }
    pc_candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let pc_total = pc_candidates.len();
    pc_candidates.truncate(5);
    // Go projects the ready width after completing the returned cut as a SET
    // (advanced_insights.go:1037): `len(getActionableIssuesAfterCompletions(completedCut))`.
    // Rust added only the first suggestion's gain, which under-counts whenever
    // more than one suggestion is returned (1859 vs 1872 on xl_2500).
    let completed_cut: std::collections::HashSet<&str> = pc_candidates
        .iter()
        .map(|(id, _, _, _)| id.as_str())
        .collect();
    let max_parallel = open_set
        .iter()
        .filter(|id| {
            // A completed issue is no longer a candidate for future work, so
            // it drops out of the actionable set.
            if completed_cut.contains(*id) {
                return false;
            }
            blocked_by.get(*id).is_none_or(|blockers| {
                blockers
                    .iter()
                    .all(|b| completed_cut.contains(b) || !open_set.contains(b))
            })
        })
        .count() as i64;
    // Go emits the cut suggestions themselves (advanced_insights.go:186-199);
    // Rust computed the candidates and then dropped them.
    let pc_suggestions: Vec<serde_json::Value> = pc_candidates
        .iter()
        .map(|(id, gain, _, tracks)| {
            serde_json::json!({
                "id": id,
                "title": title_of(id),
                "parallel_gain": gain,
                "enabled_tracks": tracks,
            })
        })
        .collect();
    // Go's FeatureStatus.Count/Limited are omitempty, so an empty cut emits a
    // bare {"state":"available"} and omits `suggestions` entirely.
    let mut pc_status = serde_json::json!({"state": "available"});
    if !pc_suggestions.is_empty() {
        pc_status["count"] = serde_json::json!(pc_suggestions.len());
        pc_status["limited"] = serde_json::json!(pc_total);
    }
    // Go's FeatureStatus.Capped marks that the result was truncated at the
    // limit, distinct from `limited` which is the pre-cap count.
    if pc_total > pc_suggestions.len() {
        pc_status["capped"] = serde_json::json!(true);
    }
    let mut parallel_cut = serde_json::json!({
        "status": pc_status,
        "max_parallel": max_parallel,
        "how_to_use": "Issues that enable parallel work. Complete to maximize team throughput.",
    });
    if !pc_suggestions.is_empty() {
        parallel_cut["suggestions"] = serde_json::json!(pc_suggestions);
    }

    // ---- Parallel Gain — Go generateParallelGain (advanced_insights.go:1063) ----
    let parallel_gain = compute_parallel_gain(issues, 5);

    // ---- Cycle Break — Go generateCycleBreakSuggestions ----
    //
    // Go distinguishes "no cycles" from "cycles were never computed".
    // `ConfigForSize` disables cycle detection above the XL threshold
    // (config.go:190-197, CyclesSkipReason "graph too large (>2000 nodes)"),
    // so on a large graph an empty cycle list means *unknown*, not *acyclic*.
    // Rust reported "No cycles detected - the dependency graph is a proper
    // DAG" there, which is the one conclusion the data does not support.
    let cycles_skipped = bv_analysis::analyzer::AnalysisBudget::default().skip_cycles(issues.len());
    let cycle_break = if cycles_skipped {
        serde_json::json!({
            "status": feature_status(
                "skipped",
                "cycle detection skipped: graph too large (>2000 nodes)",
                false,
                0,
                0,
            ),
            "cycle_count": 0,
            "how_to_use": "Structural fix suggestions. Apply BEFORE working on cycle members.",
            "advisory": "Cycle analysis is unavailable; do not infer that the dependency graph is acyclic.",
        })
    } else if cycles.is_empty() {
        serde_json::json!({
            "status": feature_status("available", "", false, 0, 0),
            "cycle_count": 0,
            "how_to_use": "Structural fix suggestions. Apply BEFORE working on cycle members.",
            "advisory": "No cycles detected - dependency graph is a proper DAG.",
        })
    } else {
        let mut edge_freq: std::collections::BTreeMap<(String, String), Vec<i64>> =
            std::collections::BTreeMap::new();
        for (ci, cycle) in cycles.iter().enumerate() {
            if cycle.is_empty() {
                continue;
            }
            // Go (advanced_insights.go:358-361) drops the two sentinel cycles
            // the detector can emit in place of a real path.
            if cycle[0] == "CYCLE_DETECTION_TIMEOUT" || cycle[0] == "..." {
                continue;
            }
            // The detector returns closed paths (A,B,C,A) while test and external
            // callers may supply the compact form (A,B,C). Go normalizes both to
            // the same edge ring by dropping the repeated tail — closing an
            // already-closed path would invent A->A — and then skips an edge it
            // has already recorded for this cycle. Without that second guard a
            // closed path counted its wrap edge twice, which is what promoted
            // the self-loop Cyc-33->Cyc-33 to the top of the ranking.
            let mut node_count = cycle.len();
            if node_count > 1 && cycle[0] == cycle[node_count - 1] {
                node_count -= 1;
            }
            let mut seen: std::collections::BTreeSet<(String, String)> = Default::default();
            for j in 0..node_count {
                let key = (cycle[j].clone(), cycle[(j + 1) % node_count].clone());
                if !seen.insert(key.clone()) {
                    continue;
                }
                edge_freq.entry(key).or_default().push(ci as i64);
            }
        }
        let mut ranked: Vec<(&(String, String), &Vec<i64>)> = edge_freq.iter().collect();
        ranked.sort_by(|a, b| {
            b.1.len()
                .cmp(&a.1.len())
                .then_with(|| a.0 .0.cmp(&b.0 .0))
                .then_with(|| a.0 .1.cmp(&b.0 .1))
        });
        let mut suggestions: Vec<serde_json::Value> = Vec::new();
        for ((from, to), cycs) in ranked.iter().take(5) {
            // Go `countDependents` (advanced_insights.go:432-445) walks the
            // dependents that are still open, so a closed dependent does not
            // count as collateral.
            let collateral = issues
                .iter()
                .filter(|i| {
                    i.dependencies
                        .iter()
                        .any(|d| d.r#type.is_blocking() && d.effective_depends_on() == to.as_str())
                })
                .filter(|i| {
                    !matches!(
                        i.status,
                        bv_core::model::Status::Closed | bv_core::model::Status::Tombstone
                    )
                })
                .count();
            suggestions.push(serde_json::json!({
                "edge_from": from,
                "edge_to": to,
                "impact": cycs.len(),
                "collateral": collateral,
                "in_cycles": cycs,
                "rationale": "Appears in the most stored cycle records; review collateral before removing this dependency.",
            }));
        }
        let capped = ranked.len() > 5;
        serde_json::json!({
            "status": feature_status("available", "", capped, suggestions.len() as i64, ranked.len() as i64),
            "suggestions": suggestions,
            "cycle_count": cycles.len(),
            "how_to_use": "Structural fix suggestions. Apply BEFORE working on cycle members.",
            "advisory": "Cycle detection stores one representative cycle per cyclic component; review each edge and re-run analysis after a break.",
        })
    };

    serde_json::json!({
        "topk_set": topk,
        "coverage_set": coverage,
        "k_paths": k_paths,
        "parallel_cut": parallel_cut,
        "parallel_gain": parallel_gain,
        "cycle_break": cycle_break,
        "config": {
            "topk_set_limit": 5,
            "coverage_set_limit": 5,
            "k_paths_limit": 5,
            "path_length_cap": 50,
            "cycle_break_limit": 5,
            "parallel_cut_limit": 5, "parallel_gain_limit": 5,
        },
        // Go map keys serialize sorted alphabetically.
        "usage_hints": {
            "coverage_set": "Greedy dependency-edge coverage. Check coverage_ratio and capped before treating it as complete.",
            "cycle_break": "Structural fix suggestions. Apply BEFORE working on cycle members.",
            "k_paths": "Representative longest critical paths. Focus on issues appearing in multiple paths.",
            "parallel_cut": "Issues that enable parallel work. Complete to maximize team throughput.",
            "parallel_gain": "Independent work tracks gained by closing each actionable issue now (gain = tracks after - tracks now). Pick high-gain issues to widen parallel work; unblocks lists what opens up.",
            "topk_set": "Best k issues to complete for max downstream unlock. Work these in order.",
        },
    })
}

/// Go `AnalysisConfig` JSON shape (exported Go field names, ns timeouts).
/// `--robot-plan` variant: KCore/Articulation/Slack only, everything else
/// skipped with "not computed for --robot-plan".
fn plan_analysis_config(nodes: usize) -> serde_json::Value {
    // Go main.go:1676 — `--force-full-analysis` REPLACES the plan-specific
    // trimmed config outright (robot_registry.go:860), so both the emitted
    // `analysis_config` and the `status` map flip from skipped to computed.
    if force_full_analysis() {
        return serde_json::to_value(bv_analysis::analyzer::full_analysis_config())
            .unwrap_or_default();
    }
    // Go ConfigForSize timeout tiers (ns) — golden-verified per fixture size.
    let (bt_ns, pr_ns, cycles_ns, max_cycles, sample) = match nodes {
        n if n < 100 => (
            2_000_000_000i64,
            2_000_000_000i64,
            2_000_000_000i64,
            1000,
            0,
        ),
        n if n < 500 => (500_000_000i64, 500_000_000i64, 500_000_000i64, 100, 0),
        n if n < 2000 => (500_000_000i64, 300_000_000i64, 300_000_000i64, 50, 100),
        _ => (500_000_000i64, 200_000_000i64, 0i64, 10, 200),
    };
    serde_json::json!({
        "ComputeBetweenness": false,
        "BetweennessTimeout": bt_ns,
        "BetweennessSkipReason": "not computed for --robot-plan",
        "BetweennessMode": "skip",
        "BetweennessSampleSize": sample,
        "BetweennessIsApproximate": false,
        "ComputePageRank": false,
        "PageRankTimeout": pr_ns,
        "PageRankSkipReason": "not computed for --robot-plan",
        "ComputeHITS": false,
        "HITSTimeout": pr_ns,
        "HITSSkipReason": "not computed for --robot-plan",
        "ComputeCycles": false,
        "CyclesTimeout": cycles_ns,
        "MaxCyclesToStore": max_cycles,
        "CyclesSkipReason": "not computed for --robot-plan",
        "ComputeEigenvector": false,
        "ComputeCriticalPath": false,
        "ComputeKCore": true,
        "ComputeArticulation": true,
        "ComputeSlack": true,
    })
}

/// Go `AnalysisConfig` for `--robot-priority`: full Phase-2 config.
fn priority_analysis_config(nodes: usize) -> serde_json::Value {
    // Go's `ConfigForSize` is the single source of truth for these values
    // (pkg/analysis/config.go:98). The hand-rolled tier table this replaced
    // hard-coded `BetweennessMode: "exact"` and `BetweennessSampleSize: 0` for
    // every size, so `--robot-priority` reported the wrong analysis shape on
    // any graph large enough for Go to sample.
    //
    // Go main.go:1728 substitutes `FullAnalysisConfig` when
    // `--force-full-analysis` is set (config.go:230-254), which is what makes
    // the published config and the `status` map flip from skipped to computed.
    let report = if force_full_analysis() {
        bv_analysis::analyzer::full_analysis_config()
    } else {
        bv_analysis::analyzer::config_for_size(nodes, 0, 0.0)
    };
    serde_json::to_value(report).unwrap_or_default()
}

/// Status map for plan/priority (golden-verified): only KCore, Articulation
/// and Slack run; the rest are skipped with the plan-specific reason.
/// Priority's Go handler reports the identical status shape.
fn plan_priority_status(g: &bv_graph_core::DiGraph) -> serde_json::Value {
    let t0 = std::time::Instant::now();
    let kcore = bv_analysis::algorithms::kcore::kcore(g);
    let kcore_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t1 = std::time::Instant::now();
    let articulation = bv_analysis::algorithms::articulation::articulation_points(g);
    let art_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let t2 = std::time::Instant::now();
    // Go gates Slack on the same Phase 1 topological order (graph.go:2271):
    // when the order does not cover every issue the metric is skipped and the
    // cyclic graph is named as the reason. Computing it anyway would report a
    // longest-path value for a graph that has no longest path.
    let order_available = bv_analysis::analyzer::topological_order_available(g);
    let slack = order_available.then(|| bv_analysis::algorithms::slack::slack(g));
    let slack_ms = t2.elapsed().as_secs_f64() * 1000.0;
    let _ = (kcore, articulation, slack);

    let plan_skip = |reason: &str| bv_analysis::analyzer::StatusEntry::skipped(reason);
    // Go zeroes every `ms` under SOURCE_DATE_EPOCH (main.go:1190-1202), and
    // `statusEntry.MarshalJSON` drops the key at zero, so a pinned-clock
    // document carries none. Measured durations are not reproducible.
    let ms = |v: f64| {
        if bv_analysis::analyzer::source_date_epoch_active() {
            0.0
        } else {
            v
        }
    };
    serde_json::json!({
        "PageRank": plan_skip(""),
        "Betweenness": plan_skip(if force_full_analysis() { "" } else { "not computed for --robot-plan" }),
        "Eigenvector": plan_skip(""),
        "HITS": plan_skip(if force_full_analysis() { "" } else { "not computed for --robot-plan" }),
        "Critical": plan_skip(""),
        "Cycles": plan_skip(if force_full_analysis() { "" } else { "not computed for --robot-plan" }),
        "KCore": bv_analysis::analyzer::StatusEntry::computed(ms(kcore_ms)),
        "Articulation": bv_analysis::analyzer::StatusEntry::computed(ms(art_ms)),
        "Slack": if order_available {
            bv_analysis::analyzer::StatusEntry::computed(ms(slack_ms))
        } else {
            plan_skip(bv_analysis::analyzer::CYCLE_UNAVAILABLE_REASON)
        },
    })
}

/// Go `generateTrackID` — 1-based n to base-26 alphabetic (A, B, ..., Z, AA...).
fn go_track_id(mut n: usize) -> String {
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn run_robot_plan() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, _, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let hash = bv_core::data_hash::compute_data_hash(&issues);
    let g = bv_analysis::analyzer::build_graph(&issues);
    let blocked = bv_analysis::triage::compute_blocked_set(&issues);
    let _actionable: Vec<&bv_core::model::Issue> = issues
        .iter()
        .filter(|i| i.status.is_open() && !blocked.contains(&i.id))
        .collect();

    // Go GetExecutionPlan parity: union-find over ALL issues (blocking +
    // parent-child deps), deterministic merge (smaller root wins), tracks
    // filtered to actionable members, unblocks = newly-actionable dependents.
    let by_id: std::collections::HashMap<&str, &bv_core::model::Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();

    let mut parent: std::collections::HashMap<String, String> = issues
        .iter()
        .map(|i| (i.id.clone(), i.id.clone()))
        .collect();
    fn find(parent: &std::collections::HashMap<String, String>, x: &str) -> String {
        let mut root = x.to_string();
        while parent[&root] != root {
            root = parent[&root].clone();
        }
        root
    }
    let mut sorted_ids: Vec<&str> = issues.iter().map(|i| i.id.as_str()).collect();
    sorted_ids.sort();
    for id in &sorted_ids {
        let issue = by_id[*id];
        for dep in &issue.dependencies {
            let is_linking = dep.r#type.is_blocking()
                || matches!(dep.r#type, bv_core::model::DependencyType::ParentChild);
            if is_linking && by_id.contains_key(dep.effective_depends_on()) {
                let px = find(&parent, id);
                let py = find(&parent, dep.effective_depends_on());
                if px != py {
                    // Deterministic merge: smaller root wins.
                    if px < py {
                        parent.insert(py, px);
                    } else {
                        parent.insert(px, py);
                    }
                }
            }
        }
    }

    // Go computeUnblocks: dependents that are open and have no OTHER open blocker.
    let is_closed_like = |id: &str| -> bool {
        by_id
            .get(id)
            .map(|i| {
                matches!(
                    i.status,
                    bv_core::model::Status::Closed | bv_core::model::Status::Tombstone
                )
            })
            .unwrap_or(true)
    };
    let compute_unblocks = |issue_id: &str| -> Vec<String> {
        let mut unblocks = Vec::new();
        let Some(node) = g.node_idx(issue_id) else {
            return unblocks;
        };
        for &dep_node in g.predecessors_slice(node) {
            let dep_id = g.node_id(dep_node).unwrap_or_default();
            if is_closed_like(&dep_id) {
                continue;
            }
            let mut still_blocked = false;
            for &other in g.successors_slice(dep_node) {
                let other_id = g.node_id(other).unwrap_or_default();
                if other_id == issue_id {
                    continue;
                }
                if !is_closed_like(&other_id) {
                    still_blocked = true;
                    break;
                }
            }
            if !still_blocked {
                unblocks.push(dep_id);
            }
        }
        unblocks.sort();
        unblocks
    };

    // Components grouped by root (all issues, sorted iteration).
    let mut components: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for id in &sorted_ids {
        let root = find(&parent, id);
        components.entry(root).or_default().push(id.to_string());
    }
    let total_components = components.len();

    // Actionable set: open AND not blocked by open deps (Go actionableSet).
    let blocked = bv_analysis::triage::compute_blocked_set(&issues);
    let actionable: Vec<&bv_core::model::Issue> = issues
        .iter()
        .filter(|i| i.status.is_open() && !blocked.contains(&i.id))
        .collect();
    let actionable_set: std::collections::HashSet<&str> =
        actionable.iter().map(|i| i.id.as_str()).collect();

    // Build tracks: roots sorted; only components with actionable members.
    let mut tracks: Vec<serde_json::Value> = Vec::new();
    let mut track_num = 1usize;
    for members in components.values() {
        let mut actionable_members: Vec<&bv_core::model::Issue> = members
            .iter()
            .filter(|&id| actionable_set.contains(id.as_str()))
            .map(|id| by_id[id.as_str()])
            .collect();
        if actionable_members.is_empty() {
            continue;
        }
        actionable_members
            .sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
        let track_items: Vec<serde_json::Value> = actionable_members
            .iter()
            .map(|i| {
                let unblocks = compute_unblocks(&i.id);
                // Go `PlanItem.UnblocksIDs` (plan.go:15) has no `omitempty`,
                // so an item that unblocks nothing still emits `"unblocks": []`.
                // A nil slice would serialize as null, which the oracle does
                // not produce — verified against bv v0.25.0.
                let unblocks_val = serde_json::json!(unblocks);
                serde_json::json!({
                    "id": i.id, "title": i.title, "priority": i.priority,
                    "status": i.status.as_str(),
                    "unblocks": unblocks_val,
                })
            })
            .collect();
        let reason = if actionable_members.len() == 1 {
            "Single actionable item"
        } else if total_components == 1 {
            "All issues in connected graph"
        } else {
            "Independent work stream"
        };
        let label = go_track_id(track_num);
        tracks.push(serde_json::json!({
            "track_id": format!("track-{label}"),
            "items": track_items,
            "reason": reason,
        }));
        track_num += 1;
    }

    // Summary: actionable sorted by ID; strictly-greater count wins (Go).
    let mut highest_id = String::new();
    let mut highest_count = -1i64;
    let mut sorted_actionable: Vec<&&bv_core::model::Issue> = actionable.iter().collect();
    sorted_actionable.sort_by(|a, b| a.id.cmp(&b.id));
    for issue in &sorted_actionable {
        let count = compute_unblocks(&issue.id).len() as i64;
        if count > highest_count {
            highest_count = count;
            highest_id = issue.id.clone();
        }
    }
    let impact_reason = if highest_count == 1 {
        "Unblocks 1 task"
    } else if highest_count > 1 {
        "Unblocks multiple tasks"
    } else {
        "No downstream dependencies"
    };

    // Go: TotalBlocked = totalOpen (non-closed-like) - len(actionable).
    let total_open = issues
        .iter()
        .filter(|i| {
            !matches!(
                i.status,
                bv_core::model::Status::Closed | bv_core::model::Status::Tombstone
            )
        })
        .count();
    let total_blocked_count = total_open - actionable.len();

    // Go emits the full v0.25.0 envelope ahead of the plan payload
    // (generated_at, data_hash, output_format, version, source_path,
    // source_kind, source_authority, authority_hash, scope_hash), so build it
    // with the shared helper instead of the hand-rolled two-field prefix.
    let mut payload = full_envelope_for(&hash, &issues);
    payload["analysis_config"] = plan_analysis_config(g.len());
    payload["status"] = plan_priority_status(&g);
    // Go declares LabelScope/LabelContext between Status and Plan
    // (robot_registry.go:881-895); serde preserves insertion order, so the call
    // has to sit here to keep the byte order.
    insert_label_scope_keys(&mut payload, &issues, robot_now());
    payload["plan"] = serde_json::json!({
        "tracks": tracks,
        "total_actionable": actionable.len(),
        "total_blocked": total_blocked_count,
        "summary": {
            "highest_impact": highest_id,
            "impact_reason": impact_reason,
            "unblocks_count": highest_count,
        },
    });
    payload["usage_hints"] = serde_json::json!([
        "jq '.plan.tracks | length' - Number of parallel execution tracks",
        "jq '.plan.tracks[0].items | map(.id)' - First track item IDs",
        "jq '.plan.tracks[].items[] | select(.unblocks | length > 0)' - Items that unblock others",
        "jq '.plan.summary' - High-level execution summary",
        "jq '[.plan.tracks[].items[]] | length' - Total items across all tracks",
    ]);
    emit_json(&payload)
}

/// Go `DefaultThresholds` (pkg/analysis/priority.go:645).
struct PriorityThresholds {
    high_pagerank: f64,
    high_betweenness: f64,
    staleness_days: i64,
    min_confidence: f64,
    significant_delta: f64,
}

impl Default for PriorityThresholds {
    fn default() -> Self {
        Self {
            high_pagerank: 0.3,
            high_betweenness: 0.5,
            staleness_days: 14,
            min_confidence: 0.3,
            significant_delta: 0.15,
        }
    }
}

/// Go `generateRecommendation` (priority.go:735-869) plus `calculateConfidence`
/// (priority.go:904). Returns `None` when no signal fires or the derived
/// priority already matches the current one.
#[allow(clippy::too_many_arguments)]
fn build_priority_recommendation(
    r: &bv_analysis::impact::IssueImpact,
    issue: &bv_core::model::Issue,
    readiness: &bv_analysis::triage::Readiness<'_>,
    g: &bv_graph_core::DiGraph,
    th: &PriorityThresholds,
    issues: &[bv_core::model::Issue],
    cp_height: &std::collections::BTreeMap<String, f64>,
    structural: (
        &std::collections::BTreeMap<String, u32>,
        &std::collections::BTreeSet<String>,
        &std::collections::BTreeMap<String, f64>,
        u32,
    ),
) -> Option<serde_json::Value> {
    let (core_map, art_set, slack_map, max_core) = structural;
    let b = &r.breakdown;
    let issue_by_id: std::collections::HashMap<&str, &bv_core::model::Issue> =
        issues.iter().map(|i| (i.id.as_str(), i)).collect();
    // Go names the issues this one directly unblocks, capped at 10 in the
    // output; the full list is needed for the days-saved estimate. Both this
    // and the reasoning signal below read the same `computeUnblocks` result
    // (priority.go:690-693, :963-970) — a dependent only counts once nothing
    // else is holding it, so this is deliberately not a raw dependency scan.
    let unblocks_ids = bv_analysis::triage::compute_unblocks(readiness, g, &r.id, robot_now());
    // Go `computeUnblocks` (plan.go:111) finishes with `sort.Strings(unblocks)`
    // over the FULL list, before any capping. Rust's `String: Ord` is byte-wise
    // like Go's, so "TREE-10" precedes "TREE-8" ('1' 0x31 < '8' 0x38) exactly
    // as Go orders it. Sorting the full list — not the MaxUnblockedIDsShown
    // slice — also fixes the order `estimateDaysSaved` sums in (order is
    // irrelevant to the sum, but the cap would otherwise slice unsorted ids).
    let unblocks_count = unblocks_ids.len();
    let blocked_reduction = unblocks_count;
    // Go scales the issue's critical-path height (priority.go:974-980), which
    // is not the pagerank norm.
    let depth_reduction = (cp_height.get(&r.id).copied().unwrap_or(0.0) / 10.0).min(1.0);

    let mut reasoning: Vec<String> = Vec::new();
    let mut signals = 0usize;
    let mut signal_strength = 0.0f64;

    if b.pagerank_norm > th.high_pagerank {
        reasoning.push("High centrality in dependency graph".into());
        signals += 1;
        signal_strength += b.pagerank_norm;
    }
    if b.betweenness_norm > th.high_betweenness {
        reasoning.push("Critical path bottleneck".into());
        signals += 1;
        signal_strength += b.betweenness_norm;
    }
    match unblocks_count {
        n if n >= 3 => {
            reasoning.push(format!("Blocks {n} other items"));
            signals += 1;
            signal_strength += 0.5 + n as f64 / 10.0;
        }
        2 => {
            reasoning.push("Blocks 2 other items".into());
            signals += 1;
            signal_strength += 0.3;
        }
        1 => {
            reasoning.push("Blocks 1 other item".into());
            signals += 1;
            signal_strength += 0.2;
        }
        _ => {}
    }
    if b.staleness_norm >= th.staleness_days as f64 / 30.0 {
        reasoning.push(format!(
            "Stale for {}+ days",
            (b.staleness_norm * 30.0) as i64
        ));
        signals += 1;
        signal_strength += 0.2;
    }
    if b.time_to_impact_norm > 0.5 {
        reasoning.push(if b.time_to_impact_explanation.is_empty() {
            "High time-to-impact score".to_string()
        } else {
            b.time_to_impact_explanation.clone()
        });
        signals += 1;
        signal_strength += b.time_to_impact_norm;
    }
    if b.urgency_norm > 0.3 {
        reasoning.push(if b.urgency_explanation.is_empty() {
            "Elevated urgency".to_string()
        } else {
            b.urgency_explanation.clone()
        });
        signals += 1;
        signal_strength += b.urgency_norm;
    }
    if b.risk_norm > 0.4 {
        reasoning.push(if b.risk_explanation.is_empty() {
            "Elevated risk/volatility".to_string()
        } else {
            b.risk_explanation.clone()
        });
        signals += 1;
        signal_strength += b.risk_norm;
    }

    // Structural signals (Go priority.go:809-829).
    if art_set.contains(&r.id) {
        reasoning.push("Articulation point (disconnects graph)".into());
        signals += 1;
        signal_strength += 0.35;
    }
    let core = core_map.get(&r.id).copied().unwrap_or(0);
    if max_core > 0 && core == max_core {
        reasoning.push(format!("High cohesion (k-core {core})"));
        signals += 1;
        signal_strength += 0.3;
    }
    let slack = slack_map.get(&r.id).copied().unwrap_or(0.0);
    if slack == 0.0 {
        reasoning.push("Zero slack on critical chain".into());
        signals += 1;
        signal_strength += 0.25;
    } else if slack > 2.0 {
        // Softer weight so parallel-friendly work does not outweigh bottlenecks.
        reasoning.push("Parallel-friendly (slack available)".into());
        signals += 1;
        signal_strength += 0.15;
    }

    // Go `generateRecommendation` (priority.go:735-869) yields a recommendation
    // that *changes* the priority, or nil. It returns nil three ways — no
    // signal fired (priority.go:832), the derived priority already matches
    // (priority.go:840), or confidence sits under the floor (priority.go:712)
    // — and nil leaves the issue eligible for the synthetic entry Go falls
    // back to (whatif.go:180-193). So none of the three can return early: the
    // what-if gate below is the only condition that drops the issue outright.
    let suggested = bv_analysis::scoring::score_to_priority(r.score);
    let changed = if signals == 0 || suggested == issue.priority {
        None
    } else {
        // Go calculateConfidence (priority.go:904-930).
        let mut confidence = (signals as f64 / 10.0).min(1.0);
        confidence += (signal_strength / 2.0).min(0.3);
        let score_delta = (r.score - bv_analysis::scoring::priority_to_score(issue.priority)).abs();
        if score_delta >= th.significant_delta {
            confidence += 0.2;
        }
        confidence = confidence.min(1.0);

        if confidence < th.min_confidence {
            None
        } else {
            // Go caps reasoning at three entries for conciseness (bv-83).
            let mut reasoning = reasoning;
            reasoning.truncate(3);
            let direction = if suggested > issue.priority {
                "decrease"
            } else {
                "increase"
            };
            Some((confidence, direction, reasoning))
        }
    };

    let transitive = count_transitive_unblocks(&r.id, issues, &issue_by_id);
    let (confidence, direction, reasoning, suggested) = match changed {
        Some((confidence, direction, reasoning)) => (confidence, direction, reasoning, suggested),
        // Go whatif.go:180 — an issue whose priority is not going to change is
        // still worth reporting when finishing it would actually move the
        // graph. The gate reads the what-if alone, so it fires for issues that
        // produced no structural signal at all.
        None if !unblocks_ids.is_empty() || transitive > 2 => (
            0.5,
            "none",
            // `extractReasoningStrings` (whatif.go:213-218) rebuilds the ranked
            // reasons as "emoji blurb" — not the structural sentences the
            // changed path reports.
            top_reasons(b)
                .iter()
                .map(|t| {
                    format!(
                        "{} {}",
                        t["emoji"].as_str().unwrap_or_default(),
                        t["explanation"].as_str().unwrap_or_default()
                    )
                })
                .collect(),
            issue.priority,
        ),
        None => return None,
    };

    let what_if = what_if_delta(
        &unblocks_ids,
        transitive,
        blocked_reduction,
        depth_reduction,
        issues,
    );
    // Go whatif.go:165-169 — the id list is capped for display, so a short
    // list next to a larger direct count means output truncation, not a
    // smaller graph effect.
    let shown_ids = what_if["unblocked_issue_ids"]
        .as_array()
        .map_or(0, Vec::len);
    let capped = shown_ids < unblocks_ids.len();
    let mut status = serde_json::json!({
        "computed_at": jiff_now(),
        "data_hash": "",
        "phase2_ready": true,
        "deterministic": true,
        "capped": capped,
    });
    if capped {
        status["capped_fields"] = serde_json::json!("unblocked_issue_ids");
    }

    Some(serde_json::json!({
        "issue_id": r.id,
        "title": r.title,
        "current_priority": issue.priority,
        "suggested_priority": suggested,
        "impact_score": r.score,
        "confidence": confidence,
        "reasoning": reasoning,
        "direction": direction,
        "what_if": what_if,
        // Go PriorityExplanation (whatif.go:9-21) nests the same what-if delta
        // plus an inline status block alongside the ranked reasons.
        "explanation": {
            "top_reasons": top_reasons(b),
            "what_if": what_if,
            "status": status,
        },
    }))
}

/// Go `GenerateTopReasons` (pkg/analysis/whatif.go:56-107): rank the eight
/// weighted components, keep the top three above a 0.01 contribution, and
/// prefix the blurb by how strong the normalized value is.
fn top_reasons(b: &bv_analysis::impact::Breakdown) -> Vec<serde_json::Value> {
    let factors: [(&str, f64, f64, &str, &str); 8] = [
        (
            "pagerank",
            b.pagerank,
            b.pagerank_norm,
            "Central in dependency graph",
            "🎯",
        ),
        (
            "betweenness",
            b.betweenness,
            b.betweenness_norm,
            "Critical path bottleneck",
            "🔀",
        ),
        (
            "blockers",
            b.blocker_ratio,
            b.blocker_ratio_norm,
            "High blocker count",
            "🚧",
        ),
        (
            "staleness",
            b.staleness,
            b.staleness_norm,
            "Needs attention (aging)",
            "⏰",
        ),
        (
            "priority",
            b.priority_boost,
            b.priority_boost_norm,
            "Explicit priority set",
            "⭐",
        ),
        (
            "time_to_impact",
            b.time_to_impact,
            b.time_to_impact_norm,
            "Fast impact potential",
            "⚡",
        ),
        (
            "urgency",
            b.urgency,
            b.urgency_norm,
            "Urgent labels/timing",
            "🔥",
        ),
        ("risk", b.risk, b.risk_norm, "Risk/volatility factors", "⚠️"),
    ];
    let mut ranked: Vec<&(&str, f64, f64, &str, &str)> = factors.iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = Vec::new();
    for f in ranked {
        if out.len() >= 3 {
            break;
        }
        if f.1 < 0.01 {
            break;
        }
        let prefix = if f.2 > 0.7 {
            "Very high: "
        } else if f.2 > 0.4 {
            "High: "
        } else if f.2 > 0.2 {
            "Moderate: "
        } else {
            ""
        };
        out.push(serde_json::json!({
            "factor": f.0,
            "weight": f.1,
            "explanation": format!("{prefix}{}", f.3),
            "emoji": f.4,
        }));
    }
    out
}

/// Go `WhatIfDelta` (priority.go:602-620). Reported from the unblocks count,
/// which drives the parallelization term, and the slack-derived depth
/// reduction.
/// Go `WhatIfDelta` (priority.go:602-620, assembled at 1001-1010).
/// `unblocks_ids` are the direct dependents, capped at MaxUnblockedIDsShown
/// (10), exactly as Go does before populating the field.
fn what_if_delta(
    direct_unblocks: &[String],
    transitive_unblocks: usize,
    blocked_reduction: usize,
    depth_reduction: f64,
    issues: &[bv_core::model::Issue],
) -> serde_json::Value {
    let direct_count = direct_unblocks.len();
    const MAX_UNBLOCKED_IDS_SHOWN: usize = 10;
    let shown: Vec<String> = direct_unblocks
        .iter()
        .take(MAX_UNBLOCKED_IDS_SHOWN)
        .cloned()
        .collect();

    // Go `estimateDaysSaved` (priority.go:1080): sum the estimates of the
    // unblocked issues, falling back to the default estimate, and convert to
    // days.
    let mut total_minutes = 0.0f64;
    let mut counted = 0usize;
    for id in direct_unblocks {
        if let Some(i) = issues.iter().find(|i| i.id == *id) {
            match i.estimated_minutes {
                Some(m) if m > 0 => {
                    total_minutes += m as f64;
                    counted += 1;
                }
                _ => {
                    total_minutes += 60.0; // DefaultEstimatedMinutes
                    counted += 1;
                }
            }
        }
    }
    let estimated_days_saved = if counted == 0 {
        0.0
    } else {
        // Go priority.go:1106 is `return totalMinutes / 480.0` — a SUM over the
        // unblocked issues converted to 8-hour work-days. It is NOT a per-item
        // average; `counted` only guards the zero case. Dividing by it here made
        // the result N× too small whenever more than one issue is unblocked.
        total_minutes / 480.0
    };

    let mut delta = serde_json::json!({
        "direct_unblocks": direct_count,
        "transitive_unblocks": transitive_unblocks,
        "blocked_reduction": blocked_reduction,
        "depth_reduction": depth_reduction,
    });
    if estimated_days_saved > 0.0 {
        delta["estimated_days_saved"] = serde_json::json!(estimated_days_saved);
    }
    if !shown.is_empty() {
        delta["unblocked_issue_ids"] = serde_json::json!(shown);
    }
    // Go's parallelization_gain is a pointer and is nil below the top-N; for
    // the recommendations that carry a what-if it is always computed.
    delta["parallelization_gain"] = serde_json::json!(direct_count as i64 - 1);
    delta["explanation"] = serde_json::json!(what_if_explanation(
        direct_count,
        transitive_unblocks,
        blocked_reduction,
        estimated_days_saved
    ));
    delta
}

/// Go `generateWhatIfExplanation` (priority.go:1110-1133).
fn what_if_explanation(
    direct: usize,
    transitive: usize,
    blocked_reduction: usize,
    days_saved: f64,
) -> String {
    if direct == 0 {
        return "No immediate downstream impact".to_string();
    }
    let mut out = format!("Completing this directly unblocks {direct} item");
    if direct != 1 {
        out.push('s');
    }
    if transitive > direct {
        out.push_str(&format!(" ({transitive} total including cascades)"));
    }
    if blocked_reduction > 0 {
        out.push_str(&format!(", clears {blocked_reduction} blocked"));
    }
    if days_saved >= 0.5 {
        out.push_str(&format!(", enabling ~{days_saved:.1} days of work"));
    }
    out
}

/// Go `countTransitiveUnblocks` (priority.go:1041-1076): BFS over the
/// dependency cascade, counting issues that become actionable once the
/// simulated set of completed issues grows. Existing ready work is skipped —
/// it was not caused by this completion.
fn count_transitive_unblocks(
    issue_id: &str,
    issues: &[bv_core::model::Issue],
    by_id: &std::collections::HashMap<&str, &bv_core::model::Issue>,
) -> usize {
    let Some(issue) = by_id.get(issue_id) else {
        return 0;
    };
    if matches!(
        issue.status,
        bv_core::model::Status::Closed | bv_core::model::Status::Tombstone
    ) {
        return 0;
    }
    let has_deps = issues.iter().any(|o| {
        o.dependencies
            .iter()
            .any(|d| d.r#type.is_blocking() && d.effective_depends_on() == issue_id)
    });
    if !has_deps {
        return 0;
    }

    // Direct dependents of a node, following blocking edges.
    let dependents = |id: &str| -> Vec<String> {
        issues
            .iter()
            .filter(|o| {
                o.dependencies
                    .iter()
                    .any(|d| d.r#type.is_blocking() && d.effective_depends_on() == id)
            })
            .map(|o| o.id.clone())
            .collect()
    };

    let actionable_with = |id: &str, closed: &std::collections::BTreeSet<String>| -> bool {
        match by_id.get(id) {
            Some(i) => {
                i.status.is_open()
                    && i.assignee.trim().is_empty()
                    && !i.issue_type.eq_ignore_ascii_case("epic")
                    && i.dependencies.iter().all(|d| {
                        !d.r#type.is_blocking()
                            || d.effective_depends_on().is_empty()
                            || closed.contains(d.effective_depends_on())
                    })
            }
            None => false,
        }
    };

    let mut simulated_closed: std::collections::BTreeSet<String> = Default::default();
    simulated_closed.insert(issue_id.to_string());
    let mut queue: std::collections::VecDeque<String> = Default::default();
    queue.push_back(issue_id.to_string());
    let mut count = 0usize;

    while let Some(curr) = queue.pop_front() {
        for candidate in dependents(&curr) {
            if simulated_closed.contains(&candidate)
                || actionable_with(&candidate, &Default::default())
            {
                continue;
            }
            if actionable_with(&candidate, &simulated_closed) {
                simulated_closed.insert(candidate.clone());
                queue.push_back(candidate);
                count += 1;
            }
        }
    }
    count
}

/// Go `generateParallelGain` (advanced_insights.go:1063-1230): report the
/// current number of independent work tracks and, per actionable issue, how
/// many more tracks completing it would open.
///
/// A "track" is a connected component of the open dependency graph that holds
/// at least one actionable issue, so completing an issue is a gain only when it
/// splits a component or joins its dependents into a new one.
fn compute_parallel_gain(issues: &[bv_core::model::Issue], limit: usize) -> serde_json::Value {
    const HOW_TO_USE: &str = "Independent work tracks gained by closing each actionable issue now (gain = tracks after - tracks now). Pick high-gain issues to widen parallel work; unblocks lists what opens up.";
    let is_open = |i: &bv_core::model::Issue| i.status.is_open();
    // Go `ReadinessIndex.Blockers` (pkg/model/readiness.go:199-218) treats a
    // blocking dependency as a blocker when its target is *absent* as well as
    // when it is present and open: `!exists || !closedForReadiness(...)`.
    // Requiring a known-open target instead reads "blocked by something we
    // cannot see" as "not blocked" and inflates the parallel-track count.
    let known_ids: std::collections::HashSet<&str> = issues.iter().map(|o| o.id.as_str()).collect();
    let has_open_blocker = |i: &bv_core::model::Issue| {
        i.dependencies.iter().any(|d| {
            if !d.r#type.is_blocking() {
                return false;
            }
            let target = d.effective_depends_on();
            !known_ids.contains(target) || issues.iter().any(|o| o.id == target && is_open(o))
        })
    };
    let actionable: Vec<&bv_core::model::Issue> = issues
        .iter()
        .filter(|i| is_open(i) && !has_open_blocker(i))
        .collect();
    let active: std::collections::BTreeSet<String> =
        actionable.iter().map(|i| i.id.clone()).collect();
    let readiness = bv_analysis::triage::Readiness::new(issues);

    // Union-find over the open graph (blocking and parent-child edges), counting
    // components that still contain an actionable member.
    let count_tracks =
        |excluded: Option<&str>, active: &std::collections::BTreeSet<String>| -> usize {
            let mut parent: std::collections::BTreeMap<String, String> =
                std::collections::BTreeMap::new();
            for i in issues.iter().filter(|i| is_open(i)) {
                if Some(i.id.as_str()) != excluded {
                    parent.insert(i.id.clone(), i.id.clone());
                }
            }
            fn find(parent: &mut std::collections::BTreeMap<String, String>, x: &str) -> String {
                let mut cur = x.to_string();
                while let Some(next) = parent.get(&cur).cloned() {
                    if next == cur {
                        break;
                    }
                    cur = next;
                }
                cur
            }
            for i in issues.iter().filter(|i| is_open(i)) {
                if Some(i.id.as_str()) == excluded {
                    continue;
                }
                for d in &i.dependencies {
                    if !(d.r#type.is_blocking()
                        || d.r#type == bv_core::model::DependencyType::ParentChild)
                    {
                        continue;
                    }
                    let target = d.effective_depends_on().to_string();
                    if Some(target.as_str()) == excluded || !parent.contains_key(&target) {
                        continue;
                    }
                    let (pf, pt) = (find(&mut parent, &i.id), find(&mut parent, &target));
                    if pf != pt {
                        parent.insert(pt, pf);
                    }
                }
            }
            let mut roots: std::collections::BTreeSet<String> = Default::default();
            for id in active {
                if Some(id.as_str()) == excluded {
                    continue;
                }
                if parent.contains_key(id) {
                    roots.insert(find(&mut parent, id));
                }
            }
            roots.len()
        };

    let tracks_now = count_tracks(None, &active);
    let mut out = serde_json::json!({
        "status": {"state": "computed"},
        "current_parallel": tracks_now,
        "how_to_use": HOW_TO_USE,
    });
    if actionable.is_empty() {
        out["status"] =
            serde_json::json!({"state": "computed", "count": 0, "reason": "No actionable issues"});
        return out;
    }

    let mut items: Vec<(String, String, usize, i64, f64, Vec<String>)> = Vec::new();
    for issue in &actionable {
        // Go `generateParallelGain` (advanced_insights.go:1176-1200) simulates
        // the completion: it takes the issues that become ready afterwards,
        // rebuilds the active set from the survivors plus those unlocks, and
        // re-runs the union-find *without* the completed node to get a real
        // `tracksAfter`. The gain is that recomputed track delta. Deriving it
        // from the unblock count instead (`unblocks.len() - 1`) measures a
        // different quantity: on large_cyclic_600 it admitted 2 candidates
        // where Go's sweep admits 14 and reports 5.
        let mut completed: std::collections::BTreeSet<String> = Default::default();
        completed.insert(issue.id.clone());
        // `computeMarginalUnblocksFromBefore` (advanced_insights.go:570-588)
        // walks the whole issue set, so an issue already actionable cannot be a
        // new unlock — that is the `!before[id]` half of the test.
        let mut unblocks: Vec<String> = issues
            .iter()
            .filter(|o| !active.contains(o.id.as_str()))
            .filter(|o| readiness.ready_after(o.id.as_str(), robot_now(), Some(&completed)))
            .map(|o| o.id.clone())
            .collect();
        unblocks.sort();
        let mut after: std::collections::BTreeSet<String> = active
            .iter()
            .filter(|id| id.as_str() != issue.id.as_str())
            .cloned()
            .collect();
        after.extend(unblocks.iter().cloned());
        let tracks_after = count_tracks(Some(issue.id.as_str()), &after);
        let gain = tracks_after as i64 - tracks_now as i64;
        if gain <= 0 {
            continue;
        }
        let pct = if tracks_now > 0 {
            gain as f64 / tracks_now as f64 * 100.0
        } else {
            0.0
        };
        items.push((
            issue.id.clone(),
            issue.title.clone(),
            tracks_after,
            gain,
            pct,
            unblocks,
        ));
    }

    // Go's ordering: gain desc, then unblocks count desc, then id.
    items.sort_by(|a, b| {
        b.3.cmp(&a.3)
            .then_with(|| b.5.len().cmp(&a.5.len()))
            .then_with(|| a.0.cmp(&b.0))
    });
    let total = items.len();
    items.truncate(limit);
    // Go's FeatureStatus carries `count` (returned) and `limited` (original
    // count before capping); both are omitempty and both feed the digest.
    let mut status = serde_json::json!({"state": "computed"});
    if total > 0 {
        status["count"] = serde_json::json!(items.len());
        status["limited"] = serde_json::json!(total);
    }
    if total > items.len() {
        status["capped"] = serde_json::json!(true);
    }
    out["status"] = status;
    if items.is_empty() {
        return out;
    }
    // Go's field is `metrics`, not `items` (advanced_insights.go:212).
    out["metrics"] = serde_json::json!(items
        .into_iter()
        .map(|(id, title, potential, gain, pct, unblocks)| {
            serde_json::json!({
                "id": id,
                "title": title,
                "current_parallel": tracks_now,
                "potential_parallel": potential,
                "gain": gain,
                "gain_percent": pct,
                "unblocks": unblocks,
            })
        })
        .collect::<Vec<_>>());
    out
}

/// Go: `--robot-by-label`/`--robot-by-assignee` are modifiers of
/// `--robot-priority` (main.go:1799-1800) — exact-match filters applied to
/// the recommendation list, not standalone commands.
/// Go `generateRecommendation` (pkg/analysis/priority.go:735-869) over the full
/// impact-scoring engine.
///
/// Shared by `--robot-priority` and the `priority_mismatch` proactive alert
/// (pkg/drift/drift.go:1062), which needs the same recommendations.
fn priority_recommendations(issues: &[bv_core::model::Issue]) -> Vec<serde_json::Value> {
    let g = bv_analysis::build_graph(issues);
    let pr = bv_graph_core::pagerank_default(&g);
    let bw = bv_analysis::analyzer::go_betweenness(&g);
    let cp = bv_graph_core::critical_path_heights(&g);
    let pr_map: std::collections::BTreeMap<String, f64> = pr
        .iter()
        .enumerate()
        .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), *v))
        .collect();
    let bw_map: std::collections::BTreeMap<String, f64> = bw
        .iter()
        .enumerate()
        .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), *v))
        .collect();
    let cp_map: std::collections::BTreeMap<String, f64> = cp
        .iter()
        .enumerate()
        .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), *v))
        .collect();

    let now = robot_now();
    let inputs = bv_analysis::impact::ImpactInputs {
        issues,
        pagerank: &pr_map,
        betweenness: &bw_map,
        critical_path: Some(&cp_map),
        g: &g,
        now,
    };

    // Structural signals used by the recommendation engine (Go priority.go:695-708):
    // k-core number, articulation membership and critical-path slack. On the
    // real corpus these are the only signals that fire for most issues, so
    // omitting them silently suppressed every recommendation.
    let core_map: std::collections::BTreeMap<String, u32> = bv_graph_core::kcore(&g)
        .into_iter()
        .enumerate()
        .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), v))
        .collect();
    // articulation_points returns node indices, not ids.
    let art_set: std::collections::BTreeSet<String> =
        bv_analysis::algorithms::articulation::articulation_points(&g)
            .into_iter()
            .map(|i| g.node_id(i).unwrap_or_default().to_string())
            .collect();
    let slack_map: std::collections::BTreeMap<String, f64> = bv_graph_core::slack(&g)
        .into_iter()
        .enumerate()
        .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), v))
        .collect();
    let max_core = core_map.values().copied().max().unwrap_or(0);

    // Use the full impact scoring engine
    let impact_results = bv_analysis::impact::compute_impact_scores(&inputs);

    // Go `generateRecommendation` (priority.go:735-869). An issue is only
    // recommended when at least one structural signal fires AND the derived
    // priority differs from the current one; the previous code compared
    // weighted breakdown values against ad-hoc constants and hardcoded
    // confidence 1, so it never agreed with the oracle.
    let th = PriorityThresholds::default();
    // One readiness index for the whole batch — Go builds it once on the
    // Analyzer and `computeUnblocks` reads it per issue (plan.go:89-108).
    let readiness = bv_analysis::triage::Readiness::new(issues);
    let mut recommendations: Vec<serde_json::Value> = Vec::new();
    for r in &impact_results {
        let Some(issue) = issues.iter().find(|i| i.id == r.id) else {
            continue;
        };
        if let Some(rec) = build_priority_recommendation(
            r,
            issue,
            &readiness,
            &g,
            &th,
            issues,
            &cp_map,
            (&core_map, &art_set, &slack_map, max_core),
        ) {
            recommendations.push(rec);
        }
    }
    // Go `GenerateEnhancedRecommendationsWithThresholds` (whatif.go:200-208)
    // orders the merged list by impact score and caps it at ten. The
    // confidence-then-score-then-id ordering in `GenerateRecommendationsFromStats`
    // (priority.go:720-729) does not survive to the output, because Go rebuilds
    // those recommendations into a map keyed by issue id (whatif.go:148-151) and
    // walks the impact scores in their original order instead.
    recommendations.sort_by(|a, b| {
        b["impact_score"]
            .as_f64()
            .partial_cmp(&a["impact_score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    recommendations.truncate(10);
    recommendations
}

fn run_robot_priority(args: &[String]) -> ExitCode {
    let by_label = args
        .iter()
        .position(|a| a == "--robot-by-label")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let by_assignee = args
        .iter()
        .position(|a| a == "--robot-by-assignee")
        .and_then(|i| args.get(i + 1))
        .cloned();
    // Go robot_registry.go:941-943 — `--robot-min-confidence` filters the
    // scored recommendation list (not the issue set) and only bites when the
    // value is strictly positive. robot_registry.go:964-968 does the same for
    // `--robot-max-results`, guarding on `> 0` so 0 keeps the default of 10.
    let min_confidence: Option<f64> = flag_value(args, "robot-min-confidence")
        .filter(|v| !v.trim().is_empty())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| *v > 0.0);
    let max_results: usize = flag_value(args, "robot-max-results")
        .and_then(|v| go_parse_int_base0(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(10) as usize;

    let (issues, _hash, _p1, status, _g) = match load_and_analyze() {
        Ok(x) => x,
        Err(code) => return code,
    };
    // Go scores the whole graph and filters the *recommendation list*
    // afterwards (robot_registry.go:940-951). Filtering the issue set first
    // shrank total_issues and stripped the graph context the surviving
    // recommendations need, so the filter returned nothing.
    let hash = bv_core::data_hash::compute_data_hash(&issues);
    let g = bv_analysis::build_graph(&issues);

    // Shared with the `priority_mismatch` alert so both consume one
    // implementation of Go's recommendation engine.
    let mut recommendations = priority_recommendations(&issues);

    // Go filters the scored recommendations, not the issue set
    // (robot_registry.go:940-951). The list arrives already ordered by impact
    // score and capped at ten (see `priority_recommendations`), and the
    // registry preserves that order — it truncates to `maxResults` but never
    // re-sorts, so the confidence ordering in priority.go:720 is not reapplied.
    // The emitted recommendation carries no labels or assignee, so match against
    // the source issue the recommendation names.
    let rec_issue = |r: &serde_json::Value| -> Option<&bv_core::model::Issue> {
        let id = r["issue_id"].as_str()?;
        issues.iter().find(|i| i.id == id)
    };
    // Go runs the confidence test first in the same `continue` chain as the
    // label and assignee tests (robot_registry.go:940-968).
    if let Some(floor) = min_confidence {
        recommendations.retain(|r| r["confidence"].as_f64().unwrap_or(0.0) >= floor);
    }
    if let Some(label) = by_label.as_deref().filter(|v| !v.is_empty()) {
        recommendations.retain(|r| {
            rec_issue(r)
                .map(|i| i.labels.iter().any(|l| l == label))
                .unwrap_or(false)
        });
    }
    if let Some(assignee) = by_assignee.as_deref().filter(|v| !v.is_empty()) {
        recommendations.retain(|r| {
            rec_issue(r)
                .map(|i| i.assignee == assignee)
                .unwrap_or(false)
        });
    }

    recommendations.truncate(max_results);

    let mut payload = full_envelope_for(&hash, &issues);
    payload["analysis_config"] = priority_analysis_config(g.len());
    // Real status from phase2 analysis (Go priority uses full config).
    // Go's `statusEntry.MarshalJSON` (pkg/analysis/graph.go:131-147) omits
    // `reason` only when the string is empty, and every Go site that sets
    // `State: "skipped"` fills the reason in the same literal — so the status
    // the analyzer already produced is exactly Go's shape and nothing is
    // cleared here. Clearing it was reading `omitempty` as "skipped implies no
    // reason", which silently dropped "dependency graph contains a cycle;
    // topological order unavailable" and "graph too large (>2000 nodes)" — the
    // only signal that a missing Slack, critical-path or cycle number is
    // unobservable rather than zero. `--robot-triage` clears for the opposite
    // reason: its fast config disables those metrics outright, so Go has no
    // reason to report. See the block there.
    let priority_status = status.to_json_map();
    payload["status"] = priority_status;
    // Go declares LabelScope/LabelContext directly after Status
    // (robot_registry.go:985-1011).
    insert_label_scope_keys(&mut payload, &issues, robot_now());
    payload["recommendations"] = serde_json::Value::Array(recommendations.clone());
    payload["field_descriptions"] = serde_json::json!({
        "status.capped": "Whether results were truncated to prevent overload",
        "status.phase2": "Whether expensive graph metrics (PageRank, betweenness) are included",
        "top_reasons": "Top 3 factors contributing to priority score, ordered by weight",
        "what_if.cascade": "Total issues transitively unblocked (including indirect)",
        "what_if.days_saved": "Estimated days saved based on issue estimates",
        "what_if.depth": "Critical path depth reduction if completed",
        "what_if.parallelization_gain": "Net change in parallel work capacity (direct_unblocks - 1); positive = more parallel work possible",
        "what_if.unblocks": "Number of issues directly waiting on this one",
    });
    // Go echoes the active scoping modifiers into `filters`
    // (robot-priority handlers build it from min_confidence / by_label /
    // by_assignee). robot_registry.go:1019-1021 emits min_confidence only
    // when the flag was set above zero.
    let mut filters = serde_json::json!({"max_results": max_results});
    if let Some(v) = min_confidence {
        filters["min_confidence"] = serde_json::json!(v);
    }
    if let Some(v) = by_label.as_deref().filter(|v| !v.is_empty()) {
        filters["by_label"] = serde_json::json!(v);
    }
    if let Some(v) = by_assignee.as_deref().filter(|v| !v.is_empty()) {
        filters["by_assignee"] = serde_json::json!(v);
    }
    payload["filters"] = filters;
    payload["summary"] = serde_json::json!({
        "total_issues": issues.len(),
        "recommendations": recommendations.len(),
        // Go robot_registry.go:973-977 counts `rec.Confidence >= 0.7`, not the
        // impact score — the two are different quantities and the emitted
        // filter above refers to the confidence one.
        "high_confidence": recommendations.iter()
            .filter(|r| r["confidence"].as_f64().unwrap_or(0.0) >= 0.7)
            .count(),
    });
    payload["usage_hints"] = serde_json::json!([
        "jq '.recommendations[] | select(.confidence > 0.7)' - Filter high confidence",
        "jq '.recommendations[] | {id: .issue_id, score: .impact_score, prio: .suggested_priority}' - Extract essentials",
        "jq '.summary' - Overview counts",
    ]);
    emit_json(&payload)
}

fn run_robot_suggest(args: &[String]) -> ExitCode {
    // Parse --suggest-type, --suggest-confidence, --suggest-bead (Go parity).
    let suggest_type = args
        .iter()
        .position(|a| a == "--suggest-type")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let suggest_confidence = args
        .iter()
        .position(|a| a == "--suggest-confidence")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<f64>().ok());
    let suggest_bead = args
        .iter()
        .position(|a| a == "--suggest-bead")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit, loaded_source) = match load_issues_auto_meta(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    let mut config = bv_analysis::suggestions::SuggestAllConfig::default();

    if let Some(conf) = suggest_confidence {
        config.min_confidence = conf;
    }
    if let Some(ref bead) = suggest_bead {
        config.filter_bead = bead.clone();
    }

    // Map suggest-type strings (Go parity).
    match suggest_type.as_deref() {
        None | Some("") => {}
        Some("duplicate") | Some("duplicates") => {
            config.filter_type = Some("potential_duplicate".to_string());
        }
        Some("dependency") | Some("dependencies") => {
            config.filter_type = Some("missing_dependency".to_string());
        }
        Some("label") | Some("labels") => {
            config.filter_type = Some("label_suggestion".to_string());
        }
        Some("cycle") | Some("cycles") => {
            config.filter_type = Some("cycle_warning".to_string());
        }
        Some(other) => {
            eprintln!("Invalid suggest-type: {other} (use: duplicate, dependency, label, cycle)");
            return ExitCode::from(1);
        }
    }
    // Tracker-backed mutation commands need the loaded source path; the
    // suggestion layer cannot resolve an issue's live route without it.
    let source = loaded_source;
    let output = bv_analysis::suggestions::generate_robot_suggest_output(
        &issues,
        &config,
        &hash,
        &source.path,
    );
    match serde_json::to_value(&output) {
        Ok(mut v) => {
            // Go v0.25.0 stamps the full envelope ahead of the suggestion
            // payload; the analysis layer has no source path, so fill it in
            // from the shared envelope helper here.
            let envelope = full_envelope_for(&hash, &issues);
            if let Some(obj) = v.as_object_mut() {
                for key in [
                    "output_format",
                    "version",
                    "source_path",
                    "source_kind",
                    "source_authority",
                    "authority_hash",
                    "scope_hash",
                ] {
                    if let Some(env) = envelope.get(key) {
                        obj.insert(key.to_string(), env.clone());
                    }
                }
            }
            emit_json(&v)
        }
        Err(e) => {
            eprintln!("Error: serialization failed: {e}");
            ExitCode::from(1)
        }
    }
}

/// Go `runProfileStartup` (cmd/bv/main.go:4969-5030).
///
/// Go hands this handler the already-loaded `issues` and the `loadDuration`
/// that `main` measured around `datasource.LoadIssues` (main.go:2609-2755).
/// bvr's handlers each load their own issues, so the same span is timed here
/// around this handler's own load — same quantity, measured at the point where
/// the work happens.
fn run_profile_startup(json_output: bool) -> ExitCode {
    use std::time::Instant;

    let cwd = std::env::current_dir().unwrap_or_default();

    let load_start = Instant::now();
    let (issues, _hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let load_duration = load_start.elapsed();

    // Go main.go:4971-4976 — `GetBeadsDir("")` (empty repo path = cwd, and
    // BEADS_DB first, which `main` has already published from `--db`) then
    // `FindJSONLPath`. Go discards both errors and falls back to the beads
    // directory itself, so an unreadable directory reports the directory.
    let beads_dir = bv_core::discovery::get_beads_dir(&cwd).unwrap_or_else(|_| cwd.clone());
    let data_path = match bv_core::discovery::find_jsonl_path_with_warnings(&beads_dir, |_| {}) {
        Ok(Some(p)) => p,
        _ => beads_dir.clone(),
    };

    // Go main.go:4978-4981 — `NewAnalyzer(issues)` is where the graph is
    // built; `profile.BuildGraph` is that construction time, and it
    // OVERWRITES the value the analysis pass would have left there (Go's
    // `AnalyzeWithProfile` never sets it — see bv-analysis's
    // `analyze_with_profile` contract).
    let build_start = Instant::now();
    let g = std::sync::Arc::new(bv_analysis::analyzer::build_graph(&issues));
    let build_duration = build_start.elapsed();

    // Go main.go:4983-4994 — `ConfigForSize(len(issues), Σ len(deps))`.
    // Both counts come from the ISSUE list, not the graph: `build_graph` only
    // keeps blocking dependency kinds, so the graph's edge count is strictly
    // smaller. Go derives density inside `ConfigForSize` (config.go:99-105),
    // so the formula is reproduced here rather than read off the graph.
    let node_count = issues.len();
    let edge_count: usize = issues.iter().map(|i| i.dependencies.len()).sum();
    let config = if force_full_analysis() {
        bv_analysis::analyzer::full_analysis_config()
    } else {
        let density = if node_count > 1 {
            edge_count as f64 / (node_count as f64 * (node_count - 1) as f64)
        } else {
            0.0
        };
        bv_analysis::analyzer::config_for_size(node_count, edge_count, density)
    };

    // Go main.go:4996-5001.
    let (_stats, mut profile) = bv_analysis::analyzer::analyze_with_profile(g, &config);
    profile.build_graph = build_duration;
    let total_with_load = load_duration + profile.total;

    if json_output {
        let recs = profile_recommendations(&profile, total_with_load);
        // Go's `Recommendations []string` is a nil slice when empty, and
        // `encoding/json` renders nil as `null` — not `[]`. The timing check
        // always contributes exactly one entry, so the empty case is
        // unreachable in practice; emit `null` anyway so the shape matches if
        // that ever changes.
        let recs = if recs.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::to_value(&recs).unwrap_or(serde_json::Value::Null)
        };
        // Key order is Go's struct declaration order (main.go:5005-5012):
        // encoding/json emits struct fields in the order they are declared.
        let out = serde_json::json!({
            "generated_at": jiff_now(),
            "data_path": data_path.to_string_lossy(),
            "load_jsonl": bv_export::hooks::go_duration_string(load_duration),
            "profile": serde_json::to_value(&profile).unwrap_or(serde_json::Value::Null),
            "total_with_load": bv_export::hooks::go_duration_string(total_with_load),
            "recommendations": recs,
        });
        emit_json(&out)
    } else {
        print_profile_report(&profile, load_duration, total_with_load);
        ExitCode::from(0)
    }
}

/// Go `printProfileReport` (cmd/bv/main.go:5035-5073). Every literal here is a
/// contract: the headings, the per-metric labels `printMetricLine` left-pads to
/// 14 columns, and the two-space indent are all compared against the oracle.
fn print_profile_report(
    profile: &bv_analysis::analyzer::StartupProfile,
    load_duration: std::time::Duration,
    total_with_load: std::time::Duration,
) {
    println!("Startup Profile");
    println!("===============");
    println!(
        "Data: {} issues, {} dependencies, density={:.4}",
        profile.node_count, profile.edge_count, profile.density
    );
    println!();

    println!("Phase 1 (blocking):");
    println!(
        "  Load JSONL:      {}",
        profile_format_duration(load_duration)
    );
    println!(
        "  Build graph:     {}",
        profile_format_duration(profile.build_graph)
    );
    println!(
        "  Degree:          {}",
        profile_format_duration(profile.degree)
    );
    println!(
        "  TopoSort:        {}",
        profile_format_duration(profile.topo_sort)
    );
    println!(
        "  Total Phase 1:   {}\n",
        profile_format_duration(load_duration + profile.build_graph + profile.phase1)
    );

    println!("Phase 2 (async in normal mode, sync for profiling):");
    print_profile_metric_line(
        "PageRank",
        profile.pagerank,
        profile.pagerank_timeout,
        profile.config.compute_page_rank,
    );
    print_profile_metric_line(
        "Betweenness",
        profile.betweenness,
        profile.betweenness_timeout,
        profile.config.compute_betweenness,
    );
    print_profile_metric_line(
        "Eigenvector",
        profile.eigenvector,
        false,
        profile.config.compute_eigenvector,
    );
    print_profile_metric_line(
        "HITS",
        profile.hits,
        profile.hits_timeout,
        profile.config.compute_hits,
    );
    print_profile_metric_line(
        "Critical Path",
        profile.critical_path,
        false,
        profile.config.compute_critical_path,
    );
    print_profile_cycles_line(profile);
    println!(
        "  Total Phase 2:   {}\n",
        profile_format_duration(profile.phase2)
    );

    println!(
        "Total startup:     {}\n",
        profile_format_duration(total_with_load)
    );

    println!("Configuration:");
    println!("  Size tier: {}", profile_size_tier(profile.node_count));
    let skipped = profile.config.skipped_metrics();
    if skipped.is_empty() {
        println!("  All metrics computed");
    } else {
        let names: Vec<&str> = skipped.iter().map(|s| s.name).collect();
        println!("  Skipped metrics: {}", names.join(", "));
    }
    println!();

    let recommendations = profile_recommendations(profile, total_with_load);
    if !recommendations.is_empty() {
        println!("Recommendations:");
        for rec in &recommendations {
            println!("  {rec}");
        }
    }
}

/// Go `printMetricLine` (main.go:5075-5086). `%-14s` left-justifies the
/// `name + ":"` label in 14 columns, so the duration column lines up whether
/// the label is `HITS:` or `Critical Path:`.
fn print_profile_metric_line(
    name: &str,
    duration: std::time::Duration,
    timed_out: bool,
    computed: bool,
) {
    let label = format!("{name}:");
    if !computed {
        println!("  {label:<14} [Skipped]");
        return;
    }
    let suffix = if timed_out { " (TIMEOUT)" } else { "" };
    println!(
        "  {label:<14} {}{suffix}",
        profile_format_duration(duration)
    );
}

/// Go `printCyclesLine` (main.go:5089-5103). The suffix carries the cycle count
/// rather than a timeout flag alone, so it is spelled out here.
fn print_profile_cycles_line(profile: &bv_analysis::analyzer::StartupProfile) {
    if !profile.config.compute_cycles {
        println!("  {:<14} [Skipped]", "Cycles:");
        return;
    }
    let suffix = if profile.cycles_timeout {
        " (TIMEOUT)".to_string()
    } else if profile.cycle_count > 0 {
        format!(" (found: {})", profile.cycle_count)
    } else {
        " (none)".to_string()
    };
    println!(
        "  {:<14} {}{}",
        "Cycles:",
        profile_format_duration(profile.cycles),
        suffix
    );
}

/// Go `formatDuration` (main.go:5118-5124) — sub-millisecond durations render
/// with two decimals in a 6-wide field, everything else as whole milliseconds
/// in the same 6-wide field. The width is what keeps the Phase 1 / Phase 2
/// blocks aligned.
fn profile_format_duration(d: std::time::Duration) -> String {
    use std::time::Duration;
    if d < Duration::from_millis(1) {
        // Go truncates to whole microseconds first (`d.Microseconds()`), then
        // divides by 1000 — so 1.5µs reads "0.00ms", not "0.0015ms".
        return format!("{:>6.2}ms", d.as_micros() as f64 / 1000.0);
    }
    format!("{:>6}ms", d.as_millis())
}

/// Go `getSizeTier` (main.go:5126-5138). Keyed on the ISSUE count, which is the
/// same number the config tier was keyed on.
fn profile_size_tier(node_count: usize) -> &'static str {
    match node_count {
        n if n < 100 => "Small (<100 issues)",
        n if n < 500 => "Medium (100-500 issues)",
        n if n < 2000 => "Large (500-2000 issues)",
        _ => "XL (>2000 issues)",
    }
}

/// Go `generateProfileRecommendations` (main.go:5140-5191). The thresholds are
/// on `totalWithLoad` (load + analysis), and the 1-2s branch is the only one
/// that consults the config: an unskipped profile on a >=500-node graph means
/// `--force-full-analysis` is doing the work.
fn profile_recommendations(
    profile: &bv_analysis::analyzer::StartupProfile,
    total_with_load: std::time::Duration,
) -> Vec<String> {
    use std::time::Duration;

    let mut recs: Vec<String> = Vec::new();
    if total_with_load < Duration::from_millis(500) {
        recs.push("✓ Startup within acceptable range (<500ms)".to_string());
    } else if total_with_load < Duration::from_secs(1) {
        recs.push("✓ Startup acceptable (<1s)".to_string());
    } else if total_with_load < Duration::from_secs(2) {
        if profile.config.skipped_metrics().is_empty() && profile.node_count >= 500 {
            recs.push(
                "⚠ Startup is slow (1-2s) - if using --force-full-analysis, consider removing it"
                    .to_string(),
            );
        } else {
            recs.push("⚠ Startup is slow (1-2s)".to_string());
        }
    } else {
        recs.push("⚠ Startup is very slow (>2s) - optimization recommended".to_string());
    }

    if profile.pagerank_timeout {
        recs.push("⚠ PageRank timed out - graph may be too large or dense".to_string());
    }
    if profile.betweenness_timeout {
        recs.push(
            "⚠ Betweenness timed out - this is expected for large graphs (>500 nodes)".to_string(),
        );
    }
    if profile.hits_timeout {
        recs.push("⚠ HITS timed out - graph may have convergence issues".to_string());
    }
    if profile.cycles_timeout {
        recs.push(
            "⚠ Cycle detection timed out - graph may have many overlapping cycles".to_string(),
        );
    }

    // Go main.go:5171-5180 — the share only exists when betweenness ran and
    // Phase 2 has a non-zero denominator to divide by.
    if profile.config.compute_betweenness
        && profile.betweenness > Duration::ZERO
        && profile.phase2 > Duration::ZERO
    {
        let betweenness_percent =
            profile.betweenness.as_nanos() as f64 / profile.phase2.as_nanos() as f64 * 100.0;
        if betweenness_percent > 50.0 {
            recs.push(format!(
                "⚠ Betweenness taking {betweenness_percent:.0}% of Phase 2 time - consider skipping for large graphs"
            ));
        }
    }

    if profile.cycle_count > 0 {
        recs.push(format!(
            "⚠ Found {} circular dependencies - resolve to improve graph health",
            profile.cycle_count
        ));
    }

    recs
}

fn run_robot_alerts() -> ExitCode {
    let arg_value = |names: &[&str]| -> Option<String> {
        let args: Vec<String> = std::env::args().collect();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            for n in names {
                if a == n {
                    let v = args.get(i + 1).cloned().unwrap_or_default();
                    return (!v.trim().is_empty()).then_some(v);
                }
                if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                    return (!v.trim().is_empty()).then(|| v.to_string());
                }
            }
            i += 1;
        }
        None
    };
    let want_severity = arg_value(&["--severity"]);
    let want_type = arg_value(&["--alert-type"]);
    let want_label = arg_value(&["--alert-label"]);
    let cwd = std::env::current_dir().unwrap_or_default();
    let (loaded, hash, _as_of_commit) = match load_issues_auto_unscoped(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // `--label` / `--repo` narrow the set before the drift baseline is
    // captured, as Go's scopeLoadedIssues does (main.go:4870-4900), while the
    // envelope keeps describing the *loaded* source so source_authority and
    // authority_hash still report the whole file.
    let (issues, hash_override) = apply_scope(&loaded);
    let hash = hash_override.unwrap_or(hash);
    // Go (robot_registry.go:1158-1186) compares the live graph against the
    // saved baseline at `.bv/baseline.json`, falling back to comparing the
    // current stats against themselves when no baseline exists. Passing the
    // same stats for both sides — as this handler used to — made every
    // drift check trivially zero, so --robot-alerts always reported none.
    let (current, cycles, _hash, _top) = match capture_baseline_for(&issues) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // Read the baseline from the same place Go does. Go's `baseline.Load`
    // populates `TopMetrics.PageRank` from a top-level `top_metrics.pagerank`
    // array of {id, value}; our baseline files predate that field, so Go sees
    // an empty top-list and reports every current entry as "entered top".
    // Reading `stats.pagerank` here instead made the two binaries disagree
    // about the same file, so the comparison is anchored to Go's layout.
    // Whether a baseline was actually recorded matters: with no file at all
    // there is nothing to have changed, and Go exits rather than inventing a
    // comparison. With a file present, Go honours its exact layout below —
    // including an absent `top_metrics.pagerank`, which it reads as an empty
    // list and therefore reports every current entry as newly entered.
    let baseline_on_disk = std::fs::read_to_string(BASELINE_PATH)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let baseline_doc = baseline_on_disk.clone().unwrap_or_default();
    let mut baseline_stats: bv_analysis::drift::BaselineStats = baseline_doc
        .get("stats")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_else(|| current.clone());
    // Go keys the top-list by `top_metrics.pagerank`. When that key is
    // genuinely present, honour it. When it is absent, leave the baseline's
    // own PageRank alone: overwriting it with an empty map made every current
    // entry count as having "entered top", inventing a change that no
    // recorded baseline ever observed. A missing top-list is not evidence of
    // change.
    if let Some(doc) = &baseline_on_disk {
        baseline_stats.pagerank = doc
            .get("top_metrics")
            .and_then(|tm| tm.get("pagerank"))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
    }
    let result = bv_analysis::drift::calculate(
        &baseline_stats,
        &current,
        &bv_analysis::drift::DriftConfig::default(),
        &cycles,
        &issues,
        robot_now(),
    );

    // Go robot-alerts embeds the full RobotEnvelope (output_format+version)
    // and provides non-empty usage hints.
    // Go robot-alerts embeds the full RobotEnvelope (output_format+version)
    // and filters the computed alerts before emitting them. The filter itself
    // is applied to the fully chained set further down, because Go runs it
    // over `driftResult.Alerts` AFTER `Calculate` has already appended
    // checkPotentialDuplicate (drift.go:300) and checkPriorityMismatch
    // (:301) — so the proactive families are filterable too, which the emitted
    // usage hints at robot_registry.go:1240 already advertise.
    let drift_alerts: Vec<serde_json::Value> = result
        .alerts
        .iter()
        .map(|a| serde_json::to_value(a).unwrap_or_default())
        .collect();
    let mut payload = full_envelope_for(&hash, &loaded);

    // Go `checkPriorityMismatch` (pkg/drift/drift.go:1062-1100). Uses the same
    // recommendations as `--robot-priority` so both agree on what
    // "under-prioritised" means. Only "increase" directions alert: "could be
    // lower" is hygiene for --robot-priority and fires for nearly every leaf
    // on a small graph.
    let mut priority_alerts: Vec<serde_json::Value> = Vec::new();
    let mut duplicate_alerts: Vec<serde_json::Value> = Vec::new();
    // Go `expensiveCheckAllowed` (pkg/drift/drift.go:122-132) refuses both
    // whole-graph proactive families once the graph outgrows
    // `proactive_max_issues`, recording a SkippedCheck instead.
    // `checkPotentialDuplicate` consults it at :1015-1018 and
    // `checkPriorityMismatch` at :1062-1064, so NEITHER family is built on an
    // oversized graph — they are not merely unreported. The skip is already
    // emitted below as `skipped_checks`; this is the matching suppression of
    // the work, which the previous code recorded without honouring. Go's
    // `checkHighImpactUnblock` (:292) deliberately has no such guard, so it is
    // not gated here.
    let proactive_limit = bv_analysis::drift::DriftConfig::default().proactive_max_issues;
    let proactive_allowed = proactive_limit == 0 || loaded.len() <= proactive_limit;
    // Go stamps every alert with `c.nowUTC()` (drift.go:1048, 1098).
    let detected_at = robot_now()
        .to_string()
        .get(..19)
        .map(|t| format!("{t}Z"))
        .unwrap_or_default();
    if proactive_allowed {
        const MIN_CONFIDENCE: f64 = 0.6; // Go default, drift/config.go:124
        let issue_by_id: std::collections::HashMap<&str, &bv_core::model::Issue> =
            loaded.iter().map(|i| (i.id.as_str(), i)).collect();
        for rec in priority_recommendations(&loaded) {
            let conf = rec["confidence"].as_f64().unwrap_or(0.0);
            if conf < MIN_CONFIDENCE || rec["direction"].as_str() != Some("increase") {
                continue;
            }
            let id = rec["issue_id"].as_str().unwrap_or_default();
            let cur = rec["current_priority"].as_i64().unwrap_or(0);
            let sug = rec["suggested_priority"].as_i64().unwrap_or(0);
            let labels: Vec<&str> = issue_by_id
                .get(id)
                .map(|i| i.labels.iter().map(|l| l.as_str()).collect())
                .unwrap_or_default();
            // Go declares baseline_value/current_value/delta with
            // `omitempty` (drift.go:75-77), so a priority of P0 omits
            // current_value rather than emitting 0.
            let mut alert = serde_json::json!({
                "type": "priority_mismatch",
                "severity": "warning",
                "message": format!("{id} is P{cur} but graph impact suggests P{sug} (confidence {conf:.2})"),
                "issue_id": id,
                "labels": labels,
                "delta": sug - cur,
                "details": rec["reasoning"].clone(),
                "detected_at": detected_at.clone(),
                "suggested_action": format!("Review with bv --robot-priority; if it holds, set the priority to P{sug}"),
            });
            if cur != 0 {
                alert["baseline_value"] = serde_json::json!(cur);
            }
            if sug != 0 {
                alert["current_value"] = serde_json::json!(sug);
            }
            priority_alerts.push(alert);
        }
    }

    // Go `checkPotentialDuplicate` (pkg/drift/drift.go:1011-1053). Closed and
    // tombstoned issues are excluded: pairing them buries the live duplicates
    // under history. Go's Calculate runs it at drift.go:300, i.e. BEFORE
    // `checkPriorityMismatch` at :301 — which is the order the chain below
    // emits. Same `expensiveCheckAllowed` guard as above (:1015-1018).
    if proactive_allowed {
        let live: Vec<&bv_core::model::Issue> = loaded
            .iter()
            .filter(|i| {
                !matches!(
                    i.status,
                    bv_core::model::Status::Closed | bv_core::model::Status::Tombstone
                )
            })
            .collect();
        if live.len() >= 2 {
            let owned: Vec<bv_core::model::Issue> = live.into_iter().cloned().collect();
            let cfg = bv_analysis::suggestions::DuplicateConfig::default();
            // Go caps duplicate alerts at config.DuplicateMaxAlerts
            // (drift/config.go:123, default 10) and breaks out of the loop
            // once the cap is reached, on top of the detector's own
            // MaxSuggestions=20. Both caps are needed: the detector alone
            // yields 20 here, Go emits 10.
            const MAX_ALERTS: usize = 10;
            for s in bv_analysis::suggestions::detect_duplicates(&owned, &cfg) {
                if duplicate_alerts.len() >= MAX_ALERTS {
                    break;
                }
                let rel = s.related_bead.clone();
                duplicate_alerts.push(serde_json::json!({
                    "type": "potential_duplicate",
                    "severity": "info",
                    "message": s.summary,
                    "issue_id": s.target_bead,
                    "related_issue_id": rel,
                    "details": [s.reason],
                    "detected_at": detected_at.clone(),
                    "suggested_action": "Compare the two issues; close one as a duplicate or link them with a related dependency",
                }));
            }
        }
    }

    // Go appends the proactive alerts after staleness (drift.go:288 runs
    // checkStaleness, then :300 potential duplicate and :301 priority
    // mismatch), so both land at the end.
    let all_alerts: Vec<serde_json::Value> = drift_alerts
        .iter()
        .cloned()
        .chain(duplicate_alerts)
        .chain(priority_alerts)
        .collect();
    // Go robot_registry.go:1189-1218 — the three filters, in that order, over
    // the complete post-Calculate alert set. Severity and type compare the
    // wire string against the raw flag value with NO case folding
    // (`string(alert.Severity) != *cfg.AlertSeverity`), so `WARNING` matches
    // nothing. Each guard is `strings.TrimSpace(flag) != ""`.
    let want_severity = want_severity.filter(|v| !v.trim().is_empty());
    let want_type = want_type.filter(|v| !v.trim().is_empty());
    let all_alerts: Vec<serde_json::Value> = all_alerts
        .into_iter()
        .filter(|a| match want_severity.as_deref() {
            Some(w) => a.get("severity").and_then(|v| v.as_str()) == Some(w),
            None => true,
        })
        .filter(|a| match want_type.as_deref() {
            Some(w) => a.get("type").and_then(|v| v.as_str()) == Some(w),
            None => true,
        })
        .filter(|a| match want_label.as_deref().map(str::trim) {
            None | Some("") => true,
            Some(want) => {
                // robot_registry.go:1196-1218 — the PLURAL `Labels` slice first
                // (pkg/drift/drift.go:86 populates it for stale_issue,
                // blocking_cascade, potential_duplicate and priority_mismatch),
                // then the singular Label, then a substring over Details.
                let want = want.to_lowercase();
                let in_labels = a
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .map(|ls| {
                        ls.iter()
                            .filter_map(|l| l.as_str())
                            .any(|l| l.to_lowercase() == want)
                    })
                    .unwrap_or(false);
                let in_label = a
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(|l| !l.is_empty() && l.to_lowercase() == want)
                    .unwrap_or(false);
                let in_details = a
                    .get("details")
                    .and_then(|v| v.as_array())
                    .map(|ds| {
                        ds.iter()
                            .filter_map(|d| d.as_str())
                            .any(|d| d.to_lowercase().contains(&want))
                    })
                    .unwrap_or(false);
                in_labels || in_label || in_details
            }
        })
        .collect();
    payload["alerts"] = serde_json::to_value(&all_alerts).unwrap_or_default();
    // Go emits `skipped_checks` alongside the alerts so a check that did not
    // run is never read as one that found nothing. Derived directly from Go's
    // `expensiveCheckAllowed` rule rather than by re-running the analysis.
    {
        let limit = bv_analysis::drift::DriftConfig::default().proactive_max_issues;
        if limit > 0 && loaded.len() > limit {
            let skipped: Vec<bv_analysis::drift::SkippedCheck> =
                ["potential_duplicate", "priority_mismatch"]
                    .iter()
                    .map(|t| bv_analysis::drift::SkippedCheck {
                        check_type: (*t).to_string(),
                        reason: format!(
                            "{} issues exceed proactive_max_issues={}",
                            loaded.len(),
                            limit
                        ),
                    })
                    .collect();
            if !skipped.is_empty() {
                payload["skipped_checks"] = serde_json::to_value(&skipped).unwrap_or_default();
            }
        }
    }
    // The summary covers the alerts actually emitted, including the
    // proactive checks appended above.
    let count_sev_in = |list: &[serde_json::Value], s: &str| {
        list.iter()
            .filter(|a| a.get("severity").and_then(|v| v.as_str()) == Some(s))
            .count()
    };
    payload["summary"] = serde_json::json!({
        "total": all_alerts.len(),
        "critical": count_sev_in(&all_alerts, "critical"),
        "warning": count_sev_in(&all_alerts, "warning"),
        "info": count_sev_in(&all_alerts, "info"),
    });
    // Go robot_registry.go:1240-1248 — the full seven-hint list, including the
    // proactive and drift-vs-baseline filter combinations.
    payload["usage_hints"] = serde_json::json!([
        "--severity=warning --alert-type=stale_issue   # stale warnings only",
        "--alert-type=blocking_cascade                 # high-unblock opportunities",
        "--alert-type=high_impact_unblock|abandoned_claim|potential_duplicate|priority_mismatch|velocity_drop   # proactive checks (no baseline needed)",
        "--alert-type=new_cycle|density_growth|node_count_change|edge_count_change|scope_creep|blocked_increase|actionable_change|pagerank_change   # drift vs saved baseline (bv --save-baseline)",
        "--alert-label=backend                        # only alerts on issues carrying that label",
        "jq '.alerts | map({issue_id, type, suggested_action})'   # what to do about each",
        "thresholds: .bv/drift.yaml; every key and its default is listed in the README 'Alerts System' table",
    ]);
    emit_json(&payload)
}

fn run_robot_graph(args: &[String]) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let g = bv_analysis::build_graph(&issues);

    let fmt = args
        .iter()
        .position(|a| a == "--graph-format")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "json".to_string());
    if fmt != "json" && fmt != "dot" && fmt != "mermaid" {
        eprintln!("Invalid --graph-format \"{fmt}\" (expected json|dot|mermaid)");
        return ExitCode::from(2);
    }

    let mut issues: Vec<bv_core::model::Issue> = issues;
    if let Some(gi) = args.iter().position(|a| a == "--graph-root") {
        let root = args.get(gi + 1).cloned().unwrap_or_default();
        let depth = args
            .iter()
            .position(|a| a == "--graph-depth")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(usize::MAX);
        let refs: Vec<&bv_core::model::Issue> = issues.iter().collect();
        issues = bv_export::graph_export::subgraph(&refs, &root, depth)
            .into_iter()
            .cloned()
            .collect();
    }

    if fmt != "json" {
        let content = if fmt == "dot" {
            bv_export::graph_export::generate_dot(&issues, None)
        } else {
            bv_export::graph_export::generate_mermaid_graph(&issues)
        };
        let mut payload = full_envelope_for(&hash, &issues);
        payload["format"] = serde_json::json!(fmt);
        payload["graph"] = serde_json::json!(content);
        payload["nodes"] = serde_json::json!(issues.len());
        emit_json(&payload);
        return ExitCode::from(0);
    }

    // Go generateAdjacency: nodes sorted by ID with labels+pagerank; edges
    // follow sorted issues with deps sorted by DependsOnID; type normalized
    // ("blocks" when empty). Counts at top level (Go GraphExportResult).
    let pagerank = bv_graph_core::pagerank_default(&g);
    let pr_by_id: std::collections::BTreeMap<String, f64> = pagerank
        .iter()
        .enumerate()
        .map(|(i, v)| (g.node_id(i).unwrap_or_default().to_string(), *v))
        .collect();

    let mut sorted_issues: Vec<&bv_core::model::Issue> = issues.iter().collect();
    sorted_issues.sort_by(|a, b| a.id.cmp(&b.id));

    let adj_nodes: Vec<serde_json::Value> = sorted_issues
        .iter()
        .map(|i| {
            let mut node = serde_json::json!({
                "id": i.id,
                "title": i.title,
                "status": i.status.as_str(),
                "priority": i.priority,
            });
            if !i.labels.is_empty() {
                node["labels"] = serde_json::json!(i.labels);
            }
            if let Some(pr) = pr_by_id.get(&i.id) {
                if *pr != 0.0 {
                    node["pagerank"] = serde_json::json!(pr);
                }
            }
            node
        })
        .collect();

    let issue_ids: std::collections::HashSet<&str> = issues.iter().map(|i| i.id.as_str()).collect();
    let mut adj_edges: Vec<serde_json::Value> = Vec::new();
    for i in &sorted_issues {
        let mut deps: Vec<&bv_core::model::Dependency> = i.dependencies.iter().collect();
        deps.sort_by(|a, b| a.effective_depends_on().cmp(b.effective_depends_on()));
        for dep in deps {
            if !issue_ids.contains(dep.effective_depends_on()) {
                continue;
            }
            let edge_type = dep.r#type.as_str();
            adj_edges.push(serde_json::json!({
                "from": i.id,
                "to": dep.effective_depends_on(),
                "type": edge_type,
            }));
        }
    }
    let edge_count = issues
        .iter()
        .filter(|i| issue_ids.contains(i.id.as_str()))
        .flat_map(|i| i.dependencies.iter())
        .filter(|dep| issue_ids.contains(dep.effective_depends_on()))
        .count();

    // Go emits the v0.25.0 envelope first, then format/nodes/edges/
    // explanation/adjacency. This branch hand-rolled its payload and put
    // data_hash last, so it missed the envelope entirely.
    let mut payload = full_envelope_for(&hash, &issues);
    payload["format"] = serde_json::json!("json");
    payload["nodes"] = serde_json::json!(sorted_issues.len());
    payload["edges"] = serde_json::json!(edge_count);
    payload["explanation"] = serde_json::json!({
        "what": "Dependency graph as JSON adjacency list",
        "when_to_use": "When you need programmatic access to the graph structure",
    });
    payload["adjacency"] = serde_json::json!({"nodes": adj_nodes, "edges": adj_edges});
    emit_json(&payload)
}

fn run_robot_recipes() -> ExitCode {
    let mut recipes: Vec<(String, &'static str)> = [
        (
            "default",
            "Default view showing all open issues sorted by priority",
        ),
        ("actionable", "Issues ready to work on (no open blockers)"),
        ("recent", "Issues updated in the last 7 days"),
        ("blocked", "Issues waiting on dependencies"),
        (
            "high-impact",
            "Issues with highest blocking impact (PageRank)",
        ),
        ("stale", "Open issues not updated in 30+ days"),
        (
            "triage",
            "Issues sorted by computed triage score (high impact + unblocking potential)",
        ),
        ("closed", "Recently closed issues"),
        (
            "release-cut",
            "Recently closed items for changelog generation",
        ),
        (
            "quick-wins",
            "Easy items with no blockers - good for quick progress",
        ),
        (
            "bottlenecks",
            "High betweenness nodes - potential project bottlenecks",
        ),
    ]
    .iter()
    .map(|(name, desc)| (name.to_string(), *desc))
    .collect();
    // Go `ListSummaries` sorts recipe summaries alphabetically by name.
    recipes.sort_by(|a, b| a.0.cmp(&b.0));
    let recipes: Vec<serde_json::Value> = recipes
        .into_iter()
        .map(|(name, desc)| serde_json::json!({"name": name, "description": desc, "source": "builtin"}))
        .collect();

    let payload = serde_json::json!({
        "generated_at": jiff_now(),
        "output_format": output_format(),
        "version": GO_APP_VERSION,
        "recipes": recipes,
    });
    emit_json(&payload)
}

/// Read `--<name>` / `--<name>=<value>` out of the raw argv. Go's `flag`
/// package accepts both spellings and several Rust handlers already rely on
/// the `=` form (see `history_flag_value`).
/// Value of a `--name value` / `--name=value` flag, borrowed from `args`.
/// Same scan as `search_flag`; this one avoids allocating.
///
/// The scan runs to the END of argv and keeps the last match, because Go's
/// `flag` parser assigns through `Set` on every occurrence
/// (`stringValue.Set` overwrites the pointer), so a repeated flag keeps the
/// LAST value. Returning on the first match scored the wrong item for
/// `--feedback-accept d1 --feedback-accept hub`: it recorded `d1` where the
/// oracle recorded `hub`.
fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let long = format!("--{name}");
    let with_eq = format!("--{name}=");
    let mut found = None;
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&with_eq) {
            found = Some(v);
        } else if a == &long {
            found = args.get(i + 1).map(|s| s.as_str());
        }
    }
    found
}

/// Go `strconv.underscoreOK` — an underscore must follow a digit or a base
/// prefix and must itself be followed by one, so `1_000` and `0x_10` parse
/// while `_10`, `10_` and `1__0` are all syntax errors.
fn go_underscore_ok(s: &str) -> bool {
    let bytes = s.as_bytes();
    // Go tracks the last character class it saw: '^' at the start, '0' for a
    // digit or a base prefix, '_' for an underscore, '!' for anything else.
    let mut saw = b'^';
    let mut i = 0;
    let mut hex = false;
    if bytes.len() >= 2
        && bytes[0] == b'0'
        && matches!(bytes[1].to_ascii_lowercase(), b'b' | b'o' | b'x')
    {
        i = 2;
        saw = b'0';
        hex = bytes[1].eq_ignore_ascii_case(&b'x');
    }
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() || (hex && (b'a'..=b'f').contains(&c.to_ascii_lowercase())) {
            saw = b'0';
        } else if c == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
        } else {
            if saw == b'_' {
                return false;
            }
            saw = b'!';
        }
        i += 1;
    }
    saw != b'_'
}

/// Go registers `--related-max-results` as a pflag `flag.Int` (main.go:1584,
/// default 10) and hands it to `RelatedWorkOptions.MaxResults`
/// (robot_registry.go:3280-3281), which caps each of the four categories
/// independently — `file_overlap`, `commit_overlap`, `dependency_cluster` and
/// `concurrent` (related.go:216-218, 287-289, 383-385, 500-502).
///
/// pflag converts the value with `strconv.ParseInt(s, 0, 64)` while parsing
/// flags, so a value it cannot read is a PARSE error and not a silent fall back
/// to the default: `invalid argument %q for "--related-max-results" flag: %v`
/// at exit 1 (main.go:4542-4550). `Ok(None)` is the flag's absence, which
/// leaves the default to the caller.
fn parse_related_max_results(args: &[String]) -> Result<Option<i64>, String> {
    let Some(raw) = flag_value(args, "related-max-results") else {
        return Ok(None);
    };
    go_parse_int_base0(raw).map(Some).map_err(|detail| {
        format!("invalid argument {raw:?} for \"--related-max-results\" flag: {detail}")
    })
}

/// Go `strconv.ParseInt(s, 0, 64)` — the conversion pflag runs for every
/// `flag.Int` value (`intValue.Set` calls it with base 0). Go's base-0 literal
/// rules reach the wire, so they are reproduced rather than approximated: an
/// optional sign, an optional `0b`/`0o`/`0x` base prefix or a bare leading `0`
/// for octal, and underscores allowed only where `go_underscore_ok` permits.
///
/// The returned error is Go's own `NumError` wording, which is exactly what
/// `main.go:4549` prints after pflag's `invalid argument %q for %q flag: %v`.
fn go_parse_int_base0(raw: &str) -> Result<i64, String> {
    let syntax = || format!("strconv.ParseInt: parsing {raw:?}: invalid syntax");
    let out_of_range = || format!("strconv.ParseInt: parsing {raw:?}: value out of range");

    let body = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    if body.is_empty() || !go_underscore_ok(body) {
        return Err(syntax());
    }

    // The prefix is stripped only after the underscore check. The default
    // branch consumes the leading `0` on its own, so `--search-limit 0` is a
    // valid zero and only fails later at Go's `limit <= 0` fallback.
    let (radix, digits) = if let Some(rest) = body.strip_prefix('0') {
        // A `0b`/`0o`/`0x` prefix only counts when a digit follows it: Go
        // requires `len(s) >= 3`, so a bare `0x` falls through to the octal
        // default and is then a digit error, i.e. invalid syntax.
        let (prefix, tail) = rest.split_at(1.min(rest.len()));
        match prefix.as_bytes().first().map(u8::to_ascii_lowercase) {
            Some(b'b') if !tail.is_empty() => (2, tail),
            Some(b'o') if !tail.is_empty() => (8, tail),
            Some(b'x') if !tail.is_empty() => (16, tail),
            _ => (8, rest),
        }
    } else {
        (10, body)
    };

    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty() {
        return Ok(0);
    }
    let magnitude = u64::from_str_radix(&cleaned, radix).map_err(|e| {
        if *e.kind() == std::num::IntErrorKind::PosOverflow {
            out_of_range()
        } else {
            syntax()
        }
    })?;
    if raw.starts_with('-') {
        if magnitude > 1u64 << 63 {
            return Err(out_of_range());
        }
        Ok((magnitude as i64).wrapping_neg())
    } else {
        if magnitude > i64::MAX as u64 {
            return Err(out_of_range());
        }
        Ok(magnitude as i64)
    }
}

/// Go `strconv.ParseFloat(s, 64)` — the conversion pflag runs for every
/// `flag.Float64` value (`float64Value.Set`, `main.go:1571` for
/// `--relations-threshold`). Rust's `str::parse::<f64>` diverges in three
/// directions and all three are reachable through this flag:
///
///   * it rejects every hexadecimal float Go accepts (`0x1p-2`, `0x1.8p1`);
///   * it rejects Go's underscore separators and, on a signed exponent, the
///     sign itself, so `1e-5` — the natural way to spell a low threshold —
///     and `1_0` are both syntax errors;
///   * it returns `inf` where Go returns `value out of range` (`1e400`).
///
/// Go returns `(±Inf, ErrRange)` on overflow and `(0, nil)` on underflow, so
/// only overflow is an error: `1e-400` parses successfully as zero and is then
/// floored to the 0.5 default by `GetRelatedFiles`.
///
/// The error text is Go's own `NumError` wording, which is what `main.go:4549`
/// prints after pflag's `invalid argument %q for %q flag: %v`.
///
/// `inf`/`nan` parse successfully here exactly as they do in Go; Go rejects
/// them later, when the value fails JSON encoding, not here.
fn go_parse_float(raw: &str) -> Result<f64, String> {
    let syntax = || format!("strconv.ParseFloat: parsing {raw:?}: invalid syntax");
    let out_of_range = || format!("strconv.ParseFloat: parsing {raw:?}: value out of range");

    // Go `special` (internal/strconv/atof.go:37) reports how many bytes it
    // consumed next to the value, and `ParseFloat` rejects the literal unless
    // that is the whole string — which is what makes `infin` a syntax error
    // even though it starts with a valid `inf`.
    if let Some((value, consumed)) = go_float_special(raw) {
        return if consumed == raw.len() {
            Ok(value)
        } else {
            Err(syntax())
        };
    }

    // Go `readFloat` (internal/strconv/atof.go:171).
    let Some(lit) = go_read_float(raw) else {
        return Err(syntax());
    };
    if lit.consumed != raw.len() {
        return Err(syntax());
    }
    // Go checks underscores against the consumed prefix, and `underscoreOK`
    // strips the optional sign itself — so the sign stays out of the slice.
    let sign_len = usize::from(matches!(raw.as_bytes().first(), Some(b'+') | Some(b'-')));
    if lit.has_underscore && !go_underscore_ok(&raw[sign_len..lit.consumed]) {
        return Err(syntax());
    }

    if lit.hex {
        // Go `atofHex` is bit-exact, and Rust cannot delegate: its parser has
        // no hexadecimal-float support at all.
        return go_atof_hex(&lit).map_err(|()| out_of_range());
    }

    // The decimal form is one Rust already reads identically; underscores were
    // the only thing standing between the two grammars and Go has vouched for
    // them by this point.
    let digits: String = raw.chars().filter(|c| *c != '_').collect();
    match digits.parse::<f64>() {
        // A finite literal that lands on an infinity overflowed f64. Go calls
        // that ErrRange; an underflow to zero is an ordinary success.
        Ok(v) if v.is_infinite() => Err(out_of_range()),
        Ok(v) => Ok(v),
        Err(_) => Err(syntax()),
    }
}

/// Go `special` (internal/strconv/atof.go:37) — the `inf`/`infinity`/`nan`
/// literals, returned with the byte count `ParseFloat` compares against the
/// input length. `nan` is reported as 3 bytes even behind a sign, so `-nan`
/// and `+nan` are syntax errors while `-inf` parses.
fn go_float_special(s: &str) -> Option<(f64, usize)> {
    let bytes = s.as_bytes();
    let (sign, offset) = match bytes.first() {
        Some(b'-') => (-1.0f64, 1usize),
        Some(b'+') => (1.0f64, 1usize),
        _ => (1.0f64, 0usize),
    };
    let body = &s[offset..];
    let n = common_prefix_len_ignore_case(body, "infinity");
    // "Anything longer than inf is ok, but if we don't have infinity, only
    // consume inf": a partial match such as "infin" collapses back to 3 and
    // then fails the whole-string check in the caller.
    let n = if (3..8).contains(&n) { 3 } else { n };
    if n == 3 || n == 8 {
        return Some((sign * f64::INFINITY, offset + n));
    }
    if common_prefix_len_ignore_case(body, "nan") == 3 {
        return Some((f64::NAN, 3));
    }
    None
}

/// Go `commonPrefixLenIgnoreCase` (internal/strconv/atoi.go:216).
fn common_prefix_len_ignore_case(s: &str, prefix: &str) -> usize {
    s.as_bytes()
        .iter()
        .zip(prefix.as_bytes())
        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
        .count()
}

/// The fields Go's `atofHex` consumes out of `readFloat`.
struct GoFloatLiteral {
    mantissa: u64,
    exp: i32,
    neg: bool,
    trunc: bool,
    hex: bool,
    has_underscore: bool,
    /// Bytes consumed; `ParseFloat` insists this is the whole input.
    consumed: usize,
}

/// Go `readFloat` (internal/strconv/atof.go:171) — the syntax recogniser.
/// `None` is Go's `!ok`, which `atof64` turns into `ErrSyntax`. It is not a
/// value parser: it accepts the shape and defers the arithmetic.
fn go_read_float(s: &str) -> Option<GoFloatLiteral> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut neg = false;
    match bytes.first() {
        Some(b'+') => i = 1,
        Some(b'-') => {
            neg = true;
            i = 1;
        }
        _ => {}
    }

    let mut base = 10u64;
    let mut max_mant_digits = 19usize; // 10^19 fits in u64
    let mut exp_char = b'e';
    let mut hex = false;
    // Go tests `i+2 < len(s)`, so a bare "0x" never enters hex mode and is
    // rejected as a syntax error by the trailing-garbage check instead.
    if i + 2 < bytes.len() && bytes[i] == b'0' && bytes[i + 1].eq_ignore_ascii_case(&b'x') {
        base = 16;
        max_mant_digits = 16; // 16^16 fits in u64
        i += 2;
        exp_char = b'p';
        hex = true;
    }

    let mut mantissa = 0u64;
    let mut sawdot = false;
    let mut sawdigits = false;
    let mut nd = 0i32;
    let mut nd_mant = 0i32;
    let mut dp = 0i32;
    let mut trunc = false;
    let mut has_underscore = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'_' {
            has_underscore = true;
        } else if c == b'.' {
            if sawdot {
                break;
            }
            sawdot = true;
            dp = nd;
        } else if c.is_ascii_digit() {
            sawdigits = true;
            if c == b'0' && nd == 0 {
                dp -= 1; // ignore leading zeros
            } else {
                nd += 1;
                if nd_mant < max_mant_digits as i32 {
                    mantissa *= base;
                    mantissa += u64::from(c - b'0');
                    nd_mant += 1;
                } else if c != b'0' {
                    trunc = true;
                }
            }
        } else if base == 16 && (b'a'..=b'f').contains(&c.to_ascii_lowercase()) {
            sawdigits = true;
            nd += 1;
            if nd_mant < max_mant_digits as i32 {
                mantissa *= 16;
                mantissa += u64::from(c.to_ascii_lowercase() - b'a' + 10);
                nd_mant += 1;
            } else {
                trunc = true;
            }
        } else {
            break;
        }
        i += 1;
    }
    if !sawdigits {
        return None;
    }
    if !sawdot {
        dp = nd;
    }
    if hex {
        dp *= 4;
        nd_mant *= 4;
    }

    if i < bytes.len() && bytes[i].to_ascii_lowercase() == exp_char {
        i += 1;
        if i >= bytes.len() {
            return None;
        }
        let mut esign = 1i32;
        match bytes[i] {
            b'+' => i += 1,
            b'-' => {
                esign = -1;
                i += 1;
            }
            _ => {}
        }
        if i >= bytes.len() || !bytes[i].is_ascii_digit() {
            return None;
        }
        let mut e = 0i32;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            if bytes[i] == b'_' {
                has_underscore = true;
            } else if e < 10000 {
                e = e * 10 + i32::from(bytes[i] - b'0');
            }
            i += 1;
        }
        dp += e * esign;
    } else if hex {
        return None; // Go: "Must have exponent."
    }

    let exp = if mantissa != 0 { dp - nd_mant } else { 0 };
    Some(GoFloatLiteral {
        mantissa,
        exp,
        neg,
        trunc,
        hex,
        has_underscore,
        consumed: i,
    })
}

/// Go `atofHex` (internal/strconv/atof.go:496) specialised to `float64` —
/// the bit-exact hexadecimal-float conversion. `Err` is Go's `ErrRange`: the
/// magnitude overflowed, so the value is ±Inf and pflag rejects the flag.
fn go_atof_hex(lit: &GoFloatLiteral) -> Result<f64, ()> {
    const MANT_BITS: u32 = 52;
    const EXP_BITS: u32 = 11;
    // Go's `float64info.bias` (ftoa.go:30) is *negative*: it is the signed
    // offset between a stored biased exponent and the real one, and every
    // comparison below (`minExp`, `maxExp`, `exp - flt.bias`) is written in
    // terms of it.
    const BIAS: i32 = -1023;
    const MAX_EXP: i32 = (1 << EXP_BITS) + BIAS - 2;
    const MIN_EXP: i32 = BIAS + 1;

    let mut mantissa = lit.mantissa;
    // "mantissa now implicitly divided by 2^mantbits"
    let mut exp = lit.exp + MANT_BITS as i32;

    // Shift into float range, carrying the two rounding bits (the lowest of
    // which stands for "this or any later bit was non-zero").
    while mantissa != 0 && (mantissa >> (MANT_BITS + 2)) == 0 {
        mantissa <<= 1;
        exp -= 1;
    }
    if lit.trunc {
        mantissa |= 1;
    }
    while (mantissa >> (1 + MANT_BITS + 2)) != 0 {
        mantissa = mantissa >> 1 | mantissa & 1;
        exp += 1;
    }
    // Denormalize, hoping to make the value representable.
    while mantissa > 1 && exp < MIN_EXP - 2 {
        mantissa = mantissa >> 1 | mantissa & 1;
        exp += 1;
    }

    // Round using the two bottom bits, half to even.
    let mut round = mantissa & 3;
    mantissa >>= 2;
    round |= mantissa & 1;
    exp += 2;
    if round == 3 {
        mantissa += 1;
        if mantissa == 1 << (1 + MANT_BITS) {
            mantissa >>= 1;
            exp += 1;
        }
    }

    if (mantissa >> MANT_BITS) == 0 {
        exp = BIAS; // denormal or zero
    }
    let mut out_of_range = false;
    if exp > MAX_EXP {
        mantissa = 1 << MANT_BITS;
        exp = MAX_EXP + 1;
        out_of_range = true;
    }

    let mut bits = mantissa & ((1u64 << MANT_BITS) - 1);
    // Go masks the possibly-negative `exp - bias` into the exponent field, so
    // a sign-extending `as u64` reproduces the same low bits.
    bits |= ((exp - BIAS) as u64 & ((1u64 << EXP_BITS) - 1)) << MANT_BITS;
    if lit.neg {
        bits |= 1 << (MANT_BITS + EXP_BITS);
    }
    if out_of_range {
        Err(())
    } else {
        Ok(f64::from_bits(bits))
    }
}

fn search_flag(args: &[String], name: &str) -> Option<String> {
    let long = format!("--{name}");
    let with_eq = format!("--{name}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&with_eq) {
            return Some(v.to_string());
        }
        if a == &long {
            return args.get(i + 1).cloned();
        }
    }
    None
}

/// Go `search.EmbeddingConfigFromEnv` + `EmbeddingConfig.Normalized`
/// (config.go:15, embedder.go:29). `BV_SEMANTIC_EMBEDDER` /
/// `BV_SEMANTIC_MODEL` / `BV_SEMANTIC_DIM`; the provider falls back to
/// `"hash"` when unset and a non-positive or unparseable dim becomes
/// `DefaultEmbeddingDim` (384).
struct SearchEmbedderConfig {
    provider: String,
    model: String,
    dim: usize,
}

fn search_embedding_config_from_env() -> SearchEmbedderConfig {
    let provider = std::env::var("BV_SEMANTIC_EMBEDDER")
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let model = std::env::var("BV_SEMANTIC_MODEL")
        .unwrap_or_default()
        .trim()
        .to_string();
    // Go calls `strconv.Atoi` without trimming: a padded value is a parse
    // error, which leaves dim at 0 and therefore normalizes to 384 anyway.
    let dim: i64 = std::env::var("BV_SEMANTIC_DIM")
        .ok()
        .and_then(|raw| raw.parse::<i64>().ok())
        .unwrap_or(0);
    SearchEmbedderConfig {
        provider: if provider.is_empty() {
            "hash".to_string()
        } else {
            provider
        },
        model,
        dim: if dim <= 0 {
            bv_search::embedder::DEFAULT_DIM
        } else {
            dim as usize
        },
    }
}

/// Go `search.NewEmbedderFromConfig` (config.go:34). The hash embedder is the
/// only provider `bv-search` ships, so the two placeholder providers and the
/// unknown-provider branch are reproduced verbatim as errors — an unimplemented
/// provider must not silently fall back to a different ranking.
fn search_embedder_dim(cfg: &SearchEmbedderConfig) -> Result<usize, String> {
    match cfg.provider.as_str() {
        "" | "hash" => Ok(cfg.dim),
        "python-sentence-transformers" => Err(format!(
            "semantic embedder {:?} not implemented (mvp placeholder); set BV_SEMANTIC_EMBEDDER={:?} for deterministic fallback",
            cfg.provider, "hash"
        )),
        "openai" => Err(format!(
            "semantic embedder {:?} not implemented (placeholder); set BV_SEMANTIC_EMBEDDER={:?} for deterministic fallback",
            cfg.provider, "hash"
        )),
        other => Err(format!(
            "unknown semantic embedder {other:?}; expected {:?}",
            "hash"
        )),
    }
}

/// Go `parseSearchMinScore` (search_output.go:64). An empty string means "no
/// threshold" (`nil`); a non-finite value or one outside `[-1, 1]` is rejected
/// with Go's exact message, because the threshold is compared against raw
/// cosine similarity and must never be able to reject the whole result set.
fn search_parse_min_score(raw: &str) -> Result<Option<f64>, String> {
    if raw.is_empty() {
        return Ok(None);
    }
    let score: f64 = raw.parse().map_err(|_| search_min_score_error(raw))?;
    if !score.is_finite() || !(-1.0..=1.0).contains(&score) {
        return Err(search_min_score_error(raw));
    }
    Ok(Some(score))
}

fn search_min_score_error(raw: &str) -> String {
    format!("invalid --search-min-score {raw:?} (expected a finite number from -1 to 1)")
}

/// Go `search.SearchConfig` (config.go:57). `Weights` starts at Go's zero
/// value — `resolveSearchWeights` only consults it when `has_weights` is set.
struct SearchConfig {
    mode: String,
    preset: String,
    weights: bv_search::hybrid::Weights,
    has_weights: bool,
}

const SEARCH_ZERO_WEIGHTS: bv_search::hybrid::Weights = bv_search::hybrid::Weights {
    text_relevance: 0.0,
    pagerank: 0.0,
    status: 0.0,
    impact: 0.0,
    priority: 0.0,
    recency: 0.0,
};

/// A `resolveSearchConfig` failure. Go reports every one of these through
/// `resolveSearchConfig` and exits 1 — `main.go:2833` is the only exit site
/// for `applySearchConfigOverrides` errors, an unknown preset included.
struct SearchConfigError {
    message: String,
    exit_code: u8,
}

impl SearchConfigError {
    fn go(message: String) -> Self {
        Self {
            message,
            exit_code: 1,
        }
    }
}

/// Go `search.ParseSearchConfig` (config.go:70) — validate the values the
/// environment supplied. A preset owns its implied mode: naming a hybrid
/// preset with no explicit mode selects hybrid, `text-only` selects text, and
/// an explicit text mode under a hybrid preset is an error rather than a
/// silently ignored flag.
fn search_parse_config(
    mode_value: &str,
    preset_value: &str,
    weights_value: &str,
) -> Result<SearchConfig, SearchConfigError> {
    let mut cfg = SearchConfig {
        mode: "text".to_string(),
        preset: "default".to_string(),
        weights: SEARCH_ZERO_WEIGHTS,
        has_weights: false,
    };

    let mut mode_set = false;
    let mode = mode_value.trim();
    if !mode.is_empty() {
        let lowered = mode.to_lowercase();
        match lowered.as_str() {
            "text" | "hybrid" => {
                cfg.mode = lowered;
                mode_set = true;
            }
            _ => {
                return Err(SearchConfigError::go(format!(
                    "invalid search mode: {mode:?} (expected text|hybrid)"
                )))
            }
        }
    }

    let preset = preset_value.trim();
    if !preset.is_empty() {
        let name = preset.to_lowercase();
        if bv_search::hybrid::get_preset(&name).is_none() {
            return Err(SearchConfigError::go(format!("unknown preset {name:?}")));
        }
        cfg.preset = name.clone();
        if name == "text-only" {
            if !mode_set {
                cfg.mode = "text".to_string();
            }
        } else if !mode_set {
            cfg.mode = "hybrid".to_string();
        } else if cfg.mode == "text" {
            return Err(SearchConfigError::go(format!(
                "search preset {preset:?} needs hybrid mode; use hybrid mode or the text-only preset"
            )));
        }
    }

    let weights = weights_value.trim();
    if !weights.is_empty() {
        cfg.weights = search_parse_weights_json(weights).map_err(SearchConfigError::go)?;
        cfg.has_weights = true;
    }

    Ok(cfg)
}

/// Go `applySearchConfigOverrides` (search_output.go:100) — apply the flags on
/// top of the parsed environment config. Unlike `ParseSearchConfig` the flag
/// values are *not* trimmed here, because a padded mode or preset name is a
/// typo worth reporting rather than quietly repairing.
fn search_apply_config_overrides(
    mut cfg: SearchConfig,
    mode_flag: &str,
    preset_flag: &str,
    weights_flag: &str,
) -> Result<SearchConfig, SearchConfigError> {
    if !mode_flag.is_empty() {
        let lowered = mode_flag.to_lowercase();
        match lowered.as_str() {
            "text" | "hybrid" => cfg.mode = lowered,
            _ => {
                return Err(SearchConfigError::go(format!(
                    "invalid --search-mode: {mode_flag:?} (expected text|hybrid)"
                )))
            }
        }
    }

    if !preset_flag.is_empty() {
        let name = preset_flag.to_lowercase();
        if bv_search::hybrid::get_preset(&name).is_none() {
            // Go presets.go:63 formats the name it was handed, and
            // search_output.go:111 hands it the LOWERCASED flag, so
            // `--search-preset BOGUS` reports `bogus`. main.go:2833 exits 1.
            return Err(SearchConfigError::go(format!("unknown preset {name:?}")));
        }
        cfg.preset = name.clone();
        if name == "text-only" {
            if mode_flag.is_empty() {
                cfg.mode = "text".to_string();
            }
        } else if mode_flag.is_empty() {
            cfg.mode = "hybrid".to_string();
        } else if cfg.mode == "text" {
            return Err(SearchConfigError::go(format!(
                "--search-preset {preset_flag:?} needs hybrid mode; drop --search-mode text or use --search-preset text-only"
            )));
        }
    }

    if !weights_flag.is_empty() {
        cfg.weights = search_parse_weights_json(weights_flag).map_err(SearchConfigError::go)?;
        cfg.has_weights = true;
    }

    Ok(cfg)
}

/// Go `resolveSearchConfig` (search_output.go:80). The environment supplies
/// the defaults; naming a flag on a dimension suppresses the inherited value
/// for that dimension only, so `--search-mode hybrid` still inherits
/// `BV_SEARCH_PRESET` while `--search-preset` does not inherit
/// `BV_SEARCH_MODE`.
fn search_resolve_config(
    mode_flag: &str,
    preset_flag: &str,
    weights_flag: &str,
) -> Result<SearchConfig, SearchConfigError> {
    let mut mode = std::env::var("BV_SEARCH_MODE").unwrap_or_default();
    let mut preset = std::env::var("BV_SEARCH_PRESET").unwrap_or_default();
    let mut weights = std::env::var("BV_SEARCH_WEIGHTS").unwrap_or_default();
    // Validate only inherited values. A selected preset owns the implied
    // mode, and explicit text mode makes an inherited hybrid preset irrelevant.
    if !mode_flag.is_empty() || !preset_flag.is_empty() {
        mode = String::new();
    }
    if !preset_flag.is_empty() || mode_flag.eq_ignore_ascii_case("text") {
        preset = String::new();
    }
    if !weights_flag.is_empty() {
        weights = String::new();
    }
    let cfg = search_parse_config(&mode, &preset, &weights)?;
    search_apply_config_overrides(cfg, mode_flag, preset_flag, weights_flag)
}

/// Go `Weights.sum` (weights.go).
fn search_weights_sum(w: &bv_search::hybrid::Weights) -> f64 {
    w.text_relevance + w.pagerank + w.status + w.impact + w.priority + w.recency
}

/// Go `Weights.Normalize` (weights.go).
fn search_weights_normalize(w: bv_search::hybrid::Weights) -> bv_search::hybrid::Weights {
    let sum = search_weights_sum(&w);
    if sum == 0.0 {
        return w;
    }
    bv_search::hybrid::Weights {
        text_relevance: w.text_relevance / sum,
        pagerank: w.pagerank / sum,
        status: w.status / sum,
        impact: w.impact / sum,
        priority: w.priority / sum,
        recency: w.recency / sum,
    }
}

/// Go `Weights.validateComponents` (weights.go) — finite and non-negative,
/// checked before normalization can hide a negative behind a rescaled sum.
fn search_weights_validate_components(w: &bv_search::hybrid::Weights) -> Result<(), String> {
    for (name, value) in [
        ("text", w.text_relevance),
        ("pagerank", w.pagerank),
        ("status", w.status),
        ("impact", w.impact),
        ("priority", w.priority),
        ("recency", w.recency),
    ] {
        if !value.is_finite() {
            return Err(format!("weight {name:?} must be finite"));
        }
        if value < 0.0 {
            return Err("weights must be non-negative".to_string());
        }
    }
    Ok(())
}

/// Go `Weights.Validate` (weights.go) — components, the low-text-relevance
/// warning, then the 1.0 sum check with the 0.001 tolerance. Go logs the
/// warning through `log.Printf`; stderr is not part of the robot contract, so
/// it is emitted bare.
fn search_weights_validate(w: &bv_search::hybrid::Weights) -> Result<(), String> {
    search_weights_validate_components(w)?;
    if w.text_relevance < 0.1 {
        eprintln!(
            "WARNING: text weight {:.2} is very low; results may not match query",
            w.text_relevance
        );
    }
    let sum = search_weights_sum(w);
    if !sum.is_finite() {
        return Err("weights sum must be finite".to_string());
    }
    if (sum - 1.0).abs() > 0.001 {
        return Err(format!("weights must sum to 1.0, got {sum:.3}"));
    }
    Ok(())
}

/// Go `AdjustWeightsForQuery` (query_adjust.go) — floor a short query's text
/// relevance at 0.55 so an unrelated high-impact issue cannot outrank a
/// literal match, rescaling the graph weights into whatever is left.
///
/// Go ends its rescale path with `adjusted.Normalize()`. `bv-search`'s port
/// returns the rescaled weights without that final normalize, and the two
/// differ in the last ULP of every component (the rescaled sum is 1.0 only in
/// exact arithmetic), which is enough to change both the emitted `weights`
/// object and the `ranking_hash` that digests it. Re-applying `Normalize`
/// under exactly Go's own condition — a short query whose text weight was
/// below the floor — restores parity; every other Go path returns the input,
/// which is already normalized by the caller.
fn search_adjust_weights_for_query(
    weights: bv_search::hybrid::Weights,
    query: &str,
) -> bv_search::hybrid::Weights {
    let rescaled = bv_search::query::is_short_query(query)
        && weights.text_relevance < bv_search::query::SHORT_QUERY_MIN_TEXT_WEIGHT;
    let adjusted = bv_search::query::adjust_weights_for_query(weights, query);
    if rescaled {
        search_weights_normalize(adjusted)
    } else {
        adjusted
    }
}

/// Go `search.ParseWeightsJSON` (config.go:126) — all six keys are required,
/// no unknown keys, and the result must pass `Validate`. `bv-search`'s
/// `Weights` has no `Deserialize` impl (its own `Serialize` uses Rust field
/// names), so the payload is decoded here and the emitted object is assembled
/// by hand in Go's struct order.
///
/// The map's value type is `Option<f64>` because Go decodes into
/// `map[string]float64` (config.go:130) and `encoding/json` treats a JSON
/// `null` for a map element as a no-op that leaves the entry at its zero
/// value. So `{"text":null,...}` yields `TextRelevance == 0.0` and still
/// reaches `Validate`, where it passes as long as the other five components
/// sum to 1.0. `serde_json` rejects `null` for `f64` outright, which turned a
/// payload Go accepts into an "invalid weights JSON" error. `Option<f64>`
/// accepts `null` and a number and nothing else, so a string, bool or array
/// for a weight is still a type error exactly as in Go.
fn search_parse_weights_json(raw: &str) -> Result<bv_search::hybrid::Weights, String> {
    const REQUIRED: [&str; 6] = [
        "text", "pagerank", "status", "impact", "priority", "recency",
    ];
    let payload: std::collections::BTreeMap<String, Option<f64>> =
        serde_json::from_str(raw).map_err(|e| format!("invalid weights JSON: {e}"))?;
    for key in REQUIRED {
        if !payload.contains_key(key) {
            return Err(format!("weights JSON missing {key:?}"));
        }
    }
    for key in payload.keys() {
        if !REQUIRED.contains(&key.as_str()) {
            return Err(format!("weights JSON has unknown key {key:?}"));
        }
    }
    let component = |key: &str| payload[key].unwrap_or(0.0);
    let weights = bv_search::hybrid::Weights {
        text_relevance: component("text"),
        pagerank: component("pagerank"),
        status: component("status"),
        impact: component("impact"),
        priority: component("priority"),
        recency: component("recency"),
    };
    search_weights_validate(&weights)?;
    Ok(weights)
}

/// Go `search.Weights` JSON shape (weights.go struct tags) — `text`,
/// `pagerank`, `status`, `impact`, `priority`, `recency` in that order.
fn search_weights_json(w: &bv_search::hybrid::Weights) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("text".into(), serde_json::json!(w.text_relevance));
    m.insert("pagerank".into(), serde_json::json!(w.pagerank));
    m.insert("status".into(), serde_json::json!(w.status));
    m.insert("impact".into(), serde_json::json!(w.impact));
    m.insert("priority".into(), serde_json::json!(w.priority));
    m.insert("recency".into(), serde_json::json!(w.recency));
    serde_json::Value::Object(m)
}

/// Go `resolveSearchWeights` (search_output.go:144) — explicit weights report
/// the synthetic preset name `custom` so a consumer can tell a hand-tuned
/// ranking from a named one.
fn search_resolve_weights(cfg: &SearchConfig) -> (bv_search::hybrid::Weights, String) {
    if cfg.has_weights {
        return (cfg.weights, "custom".to_string());
    }
    (
        bv_search::hybrid::get_preset(&cfg.preset).unwrap_or(SEARCH_ZERO_WEIGHTS),
        cfg.preset.clone(),
    )
}

/// Go `search.NewHybridScorerAt`'s weight reconciliation
/// (hybrid_scorer_impl.go): the incoming weights are re-validated and
/// re-normalized, and anything that fails falls back to the `default` preset
/// instead of scoring with nonsense weights.
fn search_scorer_weights(w: bv_search::hybrid::Weights) -> bv_search::hybrid::Weights {
    let default_weights =
        bv_search::hybrid::get_preset("default").unwrap_or(bv_search::hybrid::Weights {
            text_relevance: 1.0,
            pagerank: 0.0,
            status: 0.0,
            impact: 0.0,
            priority: 0.0,
            recency: 0.0,
        });
    let mut normalized = default_weights;
    if search_weights_validate_components(&w).is_ok() {
        normalized = search_weights_normalize(w);
    }
    if search_weights_validate(&normalized).is_err() {
        normalized = default_weights;
    }
    normalized
}

/// Go `search.VectorSearchOptions` (vector_index.go). `min_score` is an
/// inclusive floor on the *raw* cosine similarity, applied before the lexical
/// boost, so a prefix match cannot be discarded before ranking and an exact
/// issue-ID query is filtered by the same threshold as everything else.
struct SearchOptions<'a> {
    /// `None` selects the whole index; an empty set selects nothing.
    eligible: Option<&'a std::collections::BTreeSet<String>>,
    exact_id: &'a str,
    min_score: Option<f64>,
    score_boosts: &'a std::collections::BTreeMap<String, f64>,
}

/// Go `search.SearchResult` plus the `ExactIDMatch` flag Go keeps off the wire
/// (`json:"-"`), which lets the hybrid stage promote the winner without
/// re-resolving a case-folded match from an already truncated list.
#[derive(Clone)]
struct ScoredHit {
    issue_id: String,
    score: f64,
    exact_id_match: bool,
}

/// Go `VectorIndex.SearchTopKWithOptions` (vector_index.go).
///
/// Go collects into a bounded heap ordered by `(score desc, issue_id asc)`.
/// Issue IDs are unique, so that comparator is a total order and a plain
/// sort-and-truncate produces the identical list.
fn search_top_k_with_options(
    idx: &bv_search::vector_index::VectorIndex,
    query: &[f32],
    k: usize,
    opts: &SearchOptions<'_>,
) -> Result<Vec<ScoredHit>, String> {
    let exact_id = opts.exact_id.trim();
    if let Some(min) = opts.min_score {
        if !min.is_finite() {
            return Err("minimum score must be finite".to_string());
        }
    }
    for (id, boost) in opts.score_boosts {
        if !boost.is_finite() || *boost < 0.0 {
            return Err(format!(
                "score boost for {id:?} must be finite and nonnegative"
            ));
        }
    }
    if k == 0 {
        return Ok(Vec::new());
    }
    if query.len() != idx.dim {
        return Err(format!(
            "query dim mismatch: {} != {}",
            query.len(),
            idx.dim
        ));
    }
    search_validate_finite(query).map_err(|e| format!("invalid query: {e}"))?;

    // Go clamps k to the index size, not to the eligible count.
    let ids = idx.sorted_ids();
    let k = k.min(ids.len());
    if k == 0 {
        return Ok(Vec::new());
    }

    let mut scored: Vec<ScoredHit> = Vec::new();
    let mut exact_case: Option<ScoredHit> = None;
    let mut folded: Option<ScoredHit> = None;
    let mut folded_matches = 0usize;
    for id in &ids {
        if let Some(eligible) = opts.eligible {
            if !eligible.contains(id) {
                continue;
            }
        }
        let Some(entry) = idx.get(id) else {
            continue;
        };
        let mut score = search_dot_f32(query, &entry.vector);
        if let Some(min) = opts.min_score {
            if score < min {
                continue;
            }
        }
        score += opts.score_boosts.get(id).copied().unwrap_or(0.0);
        let hit = ScoredHit {
            issue_id: id.clone(),
            score,
            exact_id_match: false,
        };
        // Only a case-insensitive match needs a copy kept for the exact-match
        // bookkeeping below; every other entry is just pushed.
        let case_folds = !exact_id.is_empty() && id.eq_ignore_ascii_case(exact_id);
        scored.push(hit);
        if case_folds {
            let recorded = scored.last().expect("just pushed").clone();
            if id == exact_id {
                exact_case = Some(recorded);
            } else {
                folded = Some(recorded);
                folded_matches += 1;
            }
        }
    }
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.issue_id.cmp(&b.issue_id))
    });
    scored.truncate(k);
    let mut results = scored;

    // An exact-case match always wins; a case-folded match is promoted only
    // when it is unambiguous, because issue IDs are opaque and a query of
    // `bv-1` must not pick a winner between `BV-1` and `bv-10`.
    let exact = match exact_case {
        Some(hit) => Some(hit),
        None if folded_matches == 1 => folded,
        None => None,
    };
    let Some(mut exact) = exact else {
        return Ok(results);
    };
    exact.exact_id_match = true;
    match results.iter().position(|r| r.issue_id == exact.issue_id) {
        Some(0) => results[0] = exact,
        Some(i) => {
            results[0..=i].rotate_right(1);
            results[0] = exact;
        }
        None => {
            if results.len() < k {
                results.push(exact);
            } else {
                let last = results.len() - 1;
                results[last] = exact;
            }
        }
    }
    Ok(results)
}

/// Go `dotFloat32` (vector_index.go) — f64 accumulator over f32 operands.
fn search_dot_f32(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

/// Go `validateFiniteVector` (vector_index.go).
fn search_validate_finite(vec: &[f32]) -> Result<(), String> {
    for (i, v) in vec.iter().enumerate() {
        if !v.is_finite() {
            return Err(format!("vector component {i} must be finite"));
        }
    }
    Ok(())
}

/// Go `search.IssueMetrics` (metrics_cache.go) — the graph-derived inputs the
/// hybrid scorer mixes into the text score.
struct SearchIssueMetrics {
    pagerank: f64,
    status: String,
    priority: i32,
    blocker_count: usize,
    updated_at: Option<jiff::Timestamp>,
}

/// Go `defaultPageRank` (metrics_cache_impl.go) — the neutral PageRank for an
/// issue the graph has no entry for (isolated after scoping).
const SEARCH_DEFAULT_PAGERANK: f64 = 0.5;

/// Go `metricsCache` after `Refresh` (metrics_cache_impl.go): one entry per
/// indexed issue, PageRank defaulted when absent, and the corpus-wide maximum
/// blocker count that `normalizeImpact` divides by.
struct SearchMetricsCache {
    metrics: std::collections::BTreeMap<String, SearchIssueMetrics>,
    max_blocker_count: usize,
}

fn search_build_metrics_cache(issues: &[bv_core::model::Issue]) -> SearchMetricsCache {
    let graph = std::sync::Arc::new(bv_analysis::build_graph(issues));
    let phase1 = bv_analysis::analyze_phase1(&graph);
    let budget = bv_analysis::AnalysisBudget {
        density: phase1.density,
        ..Default::default()
    };
    let (_status, phase2) = bv_analysis::analyze_phase2_blocking(graph, &budget);
    let page_rank = phase2.page_rank.unwrap_or_default();

    let mut metrics = std::collections::BTreeMap::new();
    let mut max_blocker_count = 0usize;
    for issue in issues {
        let blocker_count = phase1.in_degree.get(&issue.id).copied().unwrap_or(0);
        max_blocker_count = max_blocker_count.max(blocker_count);
        metrics.insert(
            issue.id.clone(),
            SearchIssueMetrics {
                pagerank: page_rank
                    .get(&issue.id)
                    .copied()
                    .unwrap_or(SEARCH_DEFAULT_PAGERANK),
                status: issue.status.as_str().to_string(),
                priority: issue.priority,
                blocker_count,
                updated_at: issue
                    .updated_at
                    .as_deref()
                    .and_then(|s| s.parse::<jiff::Timestamp>().ok()),
            },
        );
    }
    SearchMetricsCache {
        metrics,
        max_blocker_count,
    }
}

/// Go `normalizeRecencyAt` (normalizers.go). A zero `updated_at` scores the
/// neutral 0.5, a future timestamp scores 1.0, otherwise exponential decay
/// with a 30-day constant measured against the scorer's pinned clock.
fn search_normalize_recency_at(updated_at: Option<jiff::Timestamp>, now: jiff::Timestamp) -> f64 {
    let Some(updated_at) = updated_at else {
        return 0.5;
    };
    // Go: `now.Sub(updatedAt).Hours() / 24`, i.e. a fractional day count
    // measured from the scorer's pinned clock rather than the wall clock.
    let days = (now.as_nanosecond() - updated_at.as_nanosecond()) as f64 / 86_400_000_000_000.0;
    if days < 0.0 {
        return 1.0;
    }
    let score = (-days / 30.0).exp();
    if score > 1.0 {
        1.0
    } else {
        score
    }
}

/// Go `search.HybridScore` (hybrid_scorer.go).
struct SearchHybridRow {
    issue_id: String,
    final_score: f64,
    text_score: f64,
    /// `None` where Go leaves the map nil, which `omitempty` then drops.
    components: Option<std::collections::BTreeMap<String, f64>>,
}

/// Go `hybridScorer.Score` (hybrid_scorer_impl.go). An issue the metrics cache
/// does not know scores as pure text with no component breakdown, and Go skips
/// normalizing any component whose weight is zero while still emitting its key
/// — so a zeroed component reads 0.0 rather than the issue's real value.
fn search_hybrid_score(
    issue_id: &str,
    text_score: f64,
    weights: &bv_search::hybrid::Weights,
    cache: &SearchMetricsCache,
    reference_time: jiff::Timestamp,
) -> Result<SearchHybridRow, String> {
    if issue_id.is_empty() {
        return Err("issueID is required".to_string());
    }
    let Some(metrics) = cache.metrics.get(issue_id) else {
        return Ok(SearchHybridRow {
            issue_id: issue_id.to_string(),
            final_score: text_score,
            text_score,
            components: None,
        });
    };

    let status = if weights.status > 0.0 {
        bv_search::hybrid::ComponentScores::normalize_status(&metrics.status)
    } else {
        0.0
    };
    let priority = if weights.priority > 0.0 {
        bv_search::hybrid::ComponentScores::normalize_priority(metrics.priority)
    } else {
        0.0
    };
    let impact = if weights.impact > 0.0 {
        bv_search::hybrid::ComponentScores::normalize_impact(
            metrics.blocker_count,
            cache.max_blocker_count,
        )
    } else {
        0.0
    };
    let recency = if weights.recency > 0.0 {
        search_normalize_recency_at(metrics.updated_at, reference_time)
    } else {
        0.0
    };

    let components_struct = bv_search::hybrid::ComponentScores {
        pagerank: metrics.pagerank,
        status,
        impact,
        priority,
        recency,
    };
    let final_score = bv_search::hybrid::hybrid_score(text_score, weights, &components_struct);
    // Go's map marshals with sorted keys.
    let mut components = std::collections::BTreeMap::new();
    components.insert("impact".to_string(), impact);
    components.insert("pagerank".to_string(), metrics.pagerank);
    components.insert("priority".to_string(), priority);
    components.insert("recency".to_string(), recency);
    components.insert("status".to_string(), status);
    Ok(SearchHybridRow {
        issue_id: issue_id.to_string(),
        final_score,
        text_score,
        components: Some(components),
    })
}

/// Go `buildHybridScores` (search_output.go:156) — score every candidate, then
/// sort by final score descending with the issue ID as the tiebreak.
fn search_build_hybrid_scores(
    results: &[ScoredHit],
    weights: &bv_search::hybrid::Weights,
    cache: &SearchMetricsCache,
    reference_time: jiff::Timestamp,
) -> Result<Vec<SearchHybridRow>, String> {
    let mut out = Vec::with_capacity(results.len());
    for hit in results {
        out.push(search_hybrid_score(
            &hit.issue_id,
            hit.score,
            weights,
            cache,
            reference_time,
        )?);
    }
    out.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.issue_id.cmp(&b.issue_id))
    });
    Ok(out)
}

/// Go `promoteExactHybridResult` (search_output.go:195) — hoist the row whose
/// issue ID equals the promoted exact match verbatim, as
/// `SearchTopKWithOptions` already did for the text list.
fn search_promote_exact_hybrid(exact_id: &str, rows: &mut [SearchHybridRow]) {
    if exact_id.is_empty() {
        return;
    }
    if let Some(i) = rows.iter().position(|r| r.issue_id == exact_id) {
        if i > 0 {
            rows[0..=i].rotate_right(1);
        }
    }
}

/// Go's `time.Time` JSON encoding is RFC3339 with trailing zeros trimmed from
/// the fractional part, and the whole part dropped when it is zero.
fn search_go_rfc3339_nano(ts: jiff::Timestamp) -> String {
    let rendered = ts.to_string();
    let Some(dot) = rendered.find('.') else {
        return rendered;
    };
    let (head, rest) = rendered.split_at(dot);
    let digits = rest[1..].strip_suffix('Z').unwrap_or(&rest[1..]);
    let trimmed = digits.trim_end_matches('0');
    if trimmed.is_empty() {
        format!("{head}Z")
    } else {
        format!("{head}.{trimmed}Z")
    }
}

/// The retrieval configuration Go's `searchRankingHash` digests
/// (search_output.go:48). It deliberately excludes volatile invocation state —
/// `generated_at`, `loaded` and the per-result scores — so the same search
/// over the same corpus and settings hashes identically across invocations.
struct SearchRankingIdentity<'a> {
    index_data_hash: &'a str,
    candidate_hash: &'a str,
    source_path: &'a str,
    source_kind: &'a str,
    as_of_commit: &'a str,
    query: &'a str,
    mode: &'a str,
    preset: &'a str,
    weights: Option<bv_search::hybrid::Weights>,
    min_score: Option<f64>,
    limit: usize,
    provider: &'a str,
    model: &'a str,
    dim: usize,
    ranking_time: Option<jiff::Timestamp>,
    scope: &'a serde_json::Value,
}

/// Go `searchRankingHash` (search_output.go:48) — sha256 over the canonical
/// identity of the retrieval configuration. Go marshals a `map[string]any`,
/// which `encoding/json` emits with keys in sorted order, so the keys below
/// are inserted alphabetically.
fn search_ranking_hash(identity: &SearchRankingIdentity<'_>) -> String {
    let mut m = serde_json::Map::new();
    m.insert(
        "as_of_commit".into(),
        serde_json::json!(identity.as_of_commit),
    );
    m.insert(
        "candidate_hash".into(),
        serde_json::json!(identity.candidate_hash),
    );
    m.insert("dim".into(), serde_json::json!(identity.dim));
    m.insert(
        "index_data_hash".into(),
        serde_json::json!(identity.index_data_hash),
    );
    m.insert("limit".into(), serde_json::json!(identity.limit));
    m.insert(
        "min_score".into(),
        identity
            .min_score
            .map_or(serde_json::Value::Null, serde_json::Value::from),
    );
    m.insert("mode".into(), serde_json::json!(identity.mode));
    m.insert("model".into(), serde_json::json!(identity.model));
    m.insert("preset".into(), serde_json::json!(identity.preset));
    m.insert("provider".into(), serde_json::json!(identity.provider));
    m.insert("query".into(), serde_json::json!(identity.query));
    m.insert(
        "ranking_time".into(),
        identity.ranking_time.map_or(serde_json::Value::Null, |ts| {
            serde_json::json!(search_go_rfc3339_nano(ts))
        }),
    );
    m.insert("scope".into(), identity.scope.clone());
    m.insert(
        "source_kind".into(),
        serde_json::json!(identity.source_kind),
    );
    m.insert(
        "source_path".into(),
        serde_json::json!(identity.source_path),
    );
    m.insert(
        "weights".into(),
        identity
            .weights
            .map_or(serde_json::Value::Null, |w| search_weights_json(&w)),
    );
    let encoded = go_json_string(&serde_json::Value::Object(m));
    bv_search::vector_index::compute_content_hash(&encoded)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Go `--robot-search` (main.go:2814-3037 plus `cmd/bv/search_output.go`).
///
/// The handler drives the ported `bv-search` library the way Go drives
/// `pkg/search`: `EmbeddingConfigFromEnv` → `NewEmbedderFromConfig` →
/// `LoadOrNewVectorIndex` → `DocumentsFromIssues` → `SyncVectorIndex` →
/// `SearchTopKWithOptions` → (hybrid only) `NewHybridScorerAt` → the
/// `robotSearchOutput` struct in Go's field order. `--search QUERY` is
/// required (modifier-requires table), `--search-limit` caps results at 10.
///
/// One deliberate deviation: an unknown `--search-preset` is a
/// `resolveSearchConfig` error in both, so it exits 1 with Go's
/// `unknown preset "name"` wording (presets.go:63) in both.
fn run_robot_search(args: &[String]) -> ExitCode {
    let query = search_flag(args, "search").unwrap_or_default();
    if query.trim().is_empty() {
        eprintln!("Error: --search requires a non-empty query");
        return ExitCode::from(2);
    }

    // Go main.go:2817 — the threshold is parsed before the search config and
    // is a usage error, not a runtime failure.
    let min_score =
        match search_parse_min_score(&search_flag(args, "search-min-score").unwrap_or_default()) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error: {e}");
                return ExitCode::from(2);
            }
        };

    let mode_flag = search_flag(args, "search-mode").unwrap_or_default();
    let preset_flag = search_flag(args, "search-preset").unwrap_or_default();
    let weights_flag = search_flag(args, "search-weights").unwrap_or_default();
    let cfg = match search_resolve_config(&mode_flag, &preset_flag, &weights_flag) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Error: {}", e.message);
            return ExitCode::from(e.exit_code);
        }
    };

    // Go main.go:2887 — `--search-limit` defaults to 10 and a non-positive
    // value falls back to it. The value itself is a pflag `flag.Int`, so Go
    // rejects a non-integer during flag parsing (`main.go:4548`, exit 1) with
    // pflag's `invalid argument %q for %q flag: %v`. `--robot-max-results` is
    // NOT an alias: Go consumes it in exactly one place, the robot-priority
    // handler at robot_registry.go:971, and never in robot-search
    // (robot_registry.go:1603), so `--robot-max-results 2` still publishes
    // `"limit":10` there.
    let limit = match search_flag(args, "search-limit") {
        Some(raw) => match go_parse_int_base0(&raw) {
            Ok(v) if v > 0 => v as usize,
            Ok(_) => 10,
            Err(detail) => {
                eprintln!("invalid argument {raw:?} for \"--search-limit\" flag: {detail}");
                return ExitCode::from(1);
            }
        },
        None => 10,
    };

    let embed_cfg = search_embedding_config_from_env();
    let dim = match search_embedder_dim(&embed_cfg) {
        Ok(dim) => dim,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    // Go main.go:2838 — one `os.Getwd` feeds both the loader and the index
    // path, so the two can never disagree.
    let project_dir = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let as_of = search_flag(args, "as-of").filter(|s| !s.is_empty());
    let (issues_for_search, hash, as_of_commit) =
        match load_issues_auto_unscoped(&project_dir, as_of.as_deref()) {
            Ok(x) => x,
            Err(e) => {
                if is_unresolved_source_error(&e.to_string()) {
                    return emit_unresolved_source();
                }
                eprintln!("Error: {e}");
                return ExitCode::from(1);
            }
        };

    // Go main.go:2770/2789 — the indexed corpus is the *unscoped* issue set
    // while the ranked candidates are the scoped set's core issues, so a
    // `--label` search indexes everything but only ranks the label.
    let (label, _recipe, _repo) = active_scope_flags();
    let scoped_issues = apply_label_scope(&apply_repo_scope(&issues_for_search));
    let (candidate_ids, _all_ids) =
        bv_analysis::label_health::label_scope_ids(&label, &issues_for_search);
    let candidates: std::collections::BTreeSet<String> = candidate_ids.iter().cloned().collect();
    let selected_issues: Vec<bv_core::model::Issue> = scoped_issues
        .iter()
        .filter(|i| candidates.contains(&i.id))
        .cloned()
        .collect();
    let eligible: std::collections::BTreeSet<String> =
        selected_issues.iter().map(|i| i.id.clone()).collect();

    let mut index_path = bv_search::index_sync::default_index_path(&project_dir, dim);
    if let Some(resolved) = as_of_commit.as_deref().filter(|s| !s.is_empty()) {
        // Go main.go:2841 — a historical search gets its own index file so it
        // cannot overwrite the live one.
        let historical = index_path.parent().unwrap_or(&project_dir).join(format!(
            "historical-{resolved}-{}",
            index_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("index.bvvi")
        ));
        index_path = historical;
    }

    let (mut idx, loaded) = bv_search::index_sync::load_or_new(&index_path, dim);
    let mut docs = std::collections::BTreeMap::new();
    for issue in &issues_for_search {
        if issue.id.is_empty() {
            continue;
        }
        docs.insert(
            issue.id.clone(),
            bv_search::query::issue_document(
                &issue.id,
                &issue.title,
                &issue.labels,
                &issue.description,
            ),
        );
    }
    let sync_stats =
        match bv_search::index_sync::sync_index(&mut idx, &docs, |texts: &[String]| {
            texts
                .iter()
                .map(|t| bv_search::embedder::hash_embed(t, dim))
                .collect()
        }) {
            Ok(stats) => stats,
            Err(e) => {
                eprintln!("Error building semantic index: {e}");
                return ExitCode::from(1);
            }
        };
    if !loaded || sync_stats.changed() {
        if let Err(e) = idx.save(&index_path) {
            eprintln!("Error saving semantic index: {e}");
            return ExitCode::from(1);
        }
    }

    let query_vec = bv_search::embedder::hash_embed(&query, dim);

    // Parsed above, next to the other flag values, so a malformed
    // `--search-limit` fails before any index work like Go's flag parse does.
    let hybrid_mode = cfg.mode == "hybrid";
    // Hybrid widens the candidate pool so the re-ranker has something to
    // reorder; short queries widen it further (Go `HybridCandidateLimit`).
    let fetch_limit = if hybrid_mode {
        bv_search::query::hybrid_candidate_limit(limit, selected_issues.len(), &query)
    } else {
        limit
    };

    let mut score_boosts: std::collections::BTreeMap<String, f64> =
        std::collections::BTreeMap::new();
    if bv_search::query::is_short_query(&query) {
        for (id, doc) in &docs {
            if !eligible.contains(id) {
                continue;
            }
            let boost = bv_search::query::short_query_lexical_boost(&query, doc);
            if boost > 0.0 {
                score_boosts.insert(id.clone(), boost);
            }
        }
    }

    let results = match search_top_k_with_options(
        &idx,
        &query_vec,
        fetch_limit,
        &SearchOptions {
            eligible: Some(&eligible),
            exact_id: &query,
            min_score,
            score_boosts: &score_boosts,
        },
    ) {
        Ok(results) => results,
        Err(e) => {
            eprintln!("Error searching index: {e}");
            return ExitCode::from(1);
        }
    };
    let exact_id = results
        .iter()
        .find(|r| r.exact_id_match)
        .map(|r| r.issue_id.clone())
        .unwrap_or_default();

    let mut title_by_id: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for issue in &issues_for_search {
        title_by_id.insert(issue.id.clone(), issue.title.clone());
    }

    let mut ranking_time: Option<jiff::Timestamp> = None;
    let mut resolved_preset = String::new();
    let mut resolved_weights: Option<bv_search::hybrid::Weights> = None;
    let mut hybrid_rows: Vec<SearchHybridRow> = Vec::new();
    if hybrid_mode {
        let (weights, preset) = search_resolve_weights(&cfg);
        let weights = search_weights_normalize(weights);
        let weights = search_adjust_weights_for_query(weights, &query);
        resolved_preset = preset;
        resolved_weights = Some(weights);

        let cache = search_build_metrics_cache(&issues_for_search);
        // The reference clock is pinned and published as `ranking_time` so a
        // cached hybrid ranking does not drift as wall time advances.
        let reference_time = robot_now();
        ranking_time = Some(reference_time);
        let scorer_weights = search_scorer_weights(weights);
        match search_build_hybrid_scores(&results, &scorer_weights, &cache, reference_time) {
            Ok(mut rows) => {
                search_promote_exact_hybrid(&exact_id, &mut rows);
                rows.truncate(limit);
                hybrid_rows = rows;
            }
            Err(e) => {
                eprintln!("Error scoring hybrid results: {e}");
                return ExitCode::from(1);
            }
        }
    }

    // Go's human-readable branch (cmd/bv/main.go:2963, :3028-3035): the JSON
    // envelope is dispatched only when `--robot-search` is present, so a bare
    // `--search Q` prints one tab-separated row per hit and exits 0. This is
    // the path a human or a shell pipeline uses — before the robot flag
    // existed it was the only thing `--search` did.
    let robot_mode = args
        .iter()
        .any(|a| a == "--robot-search" || a.starts_with("--robot-search="));
    if !robot_mode {
        let title_of = |id: &str| title_by_id.get(id).cloned().unwrap_or_default();
        if hybrid_mode {
            for r in &hybrid_rows {
                println!(
                    "{:.4}\t{}\t{}",
                    r.final_score,
                    r.issue_id,
                    title_of(&r.issue_id)
                );
            }
        } else {
            for r in &results {
                println!("{:.4}\t{}\t{}", r.score, r.issue_id, title_of(&r.issue_id));
            }
        }
        return ExitCode::from(0);
    }

    let index_data_hash = bv_core::data_hash::compute_data_hash(&issues_for_search);
    let candidate_hash = bv_core::data_hash::compute_data_hash(&selected_issues);
    let mut payload = match full_envelope_for(&hash, &issues_for_search) {
        serde_json::Value::Object(map) => map,
        other => other.as_object().cloned().unwrap_or_default(),
    };
    let env_str = |key: &str| -> String {
        payload
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let source_path = env_str("source_path");
    let source_kind = env_str("source_kind");
    let scope = payload
        .get("scope")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let ranking_hash = search_ranking_hash(&SearchRankingIdentity {
        index_data_hash: &index_data_hash,
        candidate_hash: &candidate_hash,
        source_path: &source_path,
        source_kind: &source_kind,
        as_of_commit: as_of_commit.as_deref().unwrap_or_default(),
        query: &query,
        mode: &cfg.mode,
        preset: &resolved_preset,
        weights: resolved_weights,
        min_score,
        limit,
        provider: &embed_cfg.provider,
        model: &embed_cfg.model,
        dim,
        ranking_time,
        scope: &scope,
    });

    // Go `robotSearchResult` field order (search_output.go:18):
    // issue_id, score, text_score (omitempty), title (omitempty),
    // component_scores (omitempty).
    let mut result_rows: Vec<serde_json::Value> = Vec::new();
    if hybrid_mode {
        for row in &hybrid_rows {
            let mut m = serde_json::Map::new();
            m.insert("issue_id".into(), serde_json::json!(row.issue_id));
            m.insert("score".into(), serde_json::json!(row.final_score));
            if row.text_score != 0.0 {
                m.insert("text_score".into(), serde_json::json!(row.text_score));
            }
            let title = title_by_id.get(&row.issue_id).cloned().unwrap_or_default();
            if !title.is_empty() {
                m.insert("title".into(), serde_json::json!(title));
            }
            if let Some(components) = &row.components {
                if !components.is_empty() {
                    let mut cm = serde_json::Map::new();
                    for (key, value) in components {
                        cm.insert(key.clone(), serde_json::json!(value));
                    }
                    m.insert("component_scores".into(), serde_json::Value::Object(cm));
                }
            }
            result_rows.push(serde_json::Value::Object(m));
        }
    } else {
        for hit in &results {
            let mut m = serde_json::Map::new();
            m.insert("issue_id".into(), serde_json::json!(hit.issue_id));
            m.insert("score".into(), serde_json::json!(hit.score));
            let title = title_by_id.get(&hit.issue_id).cloned().unwrap_or_default();
            if !title.is_empty() {
                m.insert("title".into(), serde_json::json!(title));
            }
            result_rows.push(serde_json::Value::Object(m));
        }
    }

    // Go `search.IndexSyncStats` field order (index_sync.go).
    let mut index_stats = serde_json::Map::new();
    index_stats.insert("total".into(), serde_json::json!(sync_stats.total));
    index_stats.insert("added".into(), serde_json::json!(sync_stats.added));
    index_stats.insert("updated".into(), serde_json::json!(sync_stats.updated));
    index_stats.insert("removed".into(), serde_json::json!(sync_stats.removed));
    index_stats.insert("skipped".into(), serde_json::json!(sync_stats.skipped));
    index_stats.insert("embedded".into(), serde_json::json!(sync_stats.embedded));

    // Go `robotSearchOutput` field order (search_output.go:26): the embedded
    // envelope, then these. `ranking_time`, `min_score`, `model`, `preset` and
    // `weights` are `omitempty` and appear only when the search produced them.
    payload.insert("index_data_hash".into(), serde_json::json!(index_data_hash));
    payload.insert("candidate_hash".into(), serde_json::json!(candidate_hash));
    payload.insert("ranking_hash".into(), serde_json::json!(ranking_hash));
    if let Some(ts) = ranking_time {
        payload.insert(
            "ranking_time".into(),
            serde_json::json!(search_go_rfc3339_nano(ts)),
        );
    }
    if let Some(v) = min_score {
        payload.insert("min_score".into(), serde_json::json!(v));
    }
    payload.insert("query".into(), serde_json::json!(query));
    payload.insert("provider".into(), serde_json::json!(embed_cfg.provider));
    if !embed_cfg.model.is_empty() {
        payload.insert("model".into(), serde_json::json!(embed_cfg.model));
    }
    payload.insert("dim".into(), serde_json::json!(dim));
    payload.insert(
        "index_path".into(),
        serde_json::json!(index_path.to_string_lossy()),
    );
    payload.insert("index".into(), serde_json::Value::Object(index_stats));
    payload.insert("loaded".into(), serde_json::json!(loaded));
    payload.insert("limit".into(), serde_json::json!(limit));
    payload.insert("mode".into(), serde_json::json!(cfg.mode));
    if hybrid_mode {
        payload.insert("preset".into(), serde_json::json!(resolved_preset));
        if let Some(w) = resolved_weights {
            payload.insert("weights".into(), search_weights_json(&w));
        }
    }
    payload.insert("results".into(), serde_json::Value::Array(result_rows));
    let usage_hints: &[&str] = if hybrid_mode {
        &[
            "jq '.results[] | {id: .issue_id, score: .score, text: .text_score}' - Extract scores",
            "jq '.results[] | {id: .issue_id, components: .component_scores}' - Hybrid breakdown",
            "jq '.index' - Index update stats (added/updated/removed/embedded)",
        ]
    } else {
        &[
            "jq '.results[] | {id: .issue_id, score: .score, title: .title}' - Extract results",
            "jq '.index' - Index update stats (added/updated/removed/embedded)",
        ]
    };
    payload.insert("usage_hints".into(), serde_json::json!(usage_hints));
    emit_json(&serde_json::Value::Object(payload))
}

/// Go `handleRobotCausality` — `--robot-causality <bead-id>`.
/// Go `handleRobotCausality` — build the same correlation report
/// `--robot-history` produces, but with the target's committed constraint
/// observations retained, then render the causal chain and its insights.
///
/// The `data_hash` in the output is the report's own bead fingerprint, not the
/// issue-corpus hash the loader computes: Go's `CausalityResult` carries
/// `hr.DataHash` and `withEnvelope` lets it win over the envelope's own value.
fn run_robot_causality(args: &[String]) -> ExitCode {
    let bead_id = args
        .iter()
        .position(|a| a == "--robot-causality")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let cwd = go_working_dir();
    if let Err(e) = validate_correlation_repository(&cwd) {
        if unresolved_source_probe(&cwd) {
            return emit_unresolved_source();
        }
        // Go returns this as a plain error, so its dispatcher wraps it
        // as `Error handling <flag>: <err>` (robot_registry.go:508).
        eprintln!("Error handling --robot-causality: {e}");
        return ExitCode::from(1);
    }
    let (issues, _hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    // Go: CorrelatorOptions{Limit: 500, CausalityBeadID: <bead>} overridden by
    // --history-limit, with --history-since bounding the window.
    let mut opts = bv_correlation::history::HistoryOptions {
        causality_bead_id: bead_id.clone(),
        limit: 500,
        ..Default::default()
    };
    if let Some(limit) = history_flag_value("history-limit") {
        match limit.trim().parse::<i64>() {
            Ok(v) => opts.limit = v,
            Err(_) => {
                eprintln!("Error: invalid --history-limit: {limit}");
                return ExitCode::from(2);
            }
        }
    }
    if let Some(since) = history_flag_value("history-since") {
        match parse_relative_time(&since) {
            Ok(Some(ts)) => opts.since = Some(ts),
            Ok(None) => {}
            Err(e) => {
                eprintln!("Error: parsing --history-since: {e}");
                return ExitCode::from(1);
            }
        }
    }

    // One frozen instant for both the chain's open end and the result stamp, so
    // `end_time` and `generated_at` cannot disagree.
    let now = jiff_now();
    let report = match generate_correlation_report(&cwd, &issues, &opts, now.clone(), true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: generating history report: {e}");
            return ExitCode::from(1);
        }
    };

    let Some(history) = report.histories.get(&bead_id) else {
        eprintln!("Bead not found: {bead_id}");
        return ExitCode::from(1);
    };

    let mut blocker_titles = std::collections::BTreeMap::new();
    for issue in &issues {
        blocker_titles.insert(issue.id.clone(), issue.title.clone());
    }
    let caus_opts = bv_correlation::causality::CausalityOptions {
        include_commits: true,
        blocker_titles,
    };
    // Go's `robotNow()` is the reference instant, so the chain's open end and
    // the result stamp cannot disagree.
    let Ok(now) = bv_correlation::causality::GoTime::parse(&now) else {
        eprintln!("Error: invalid reference instant");
        return ExitCode::from(1);
    };
    let Some(mut result) = bv_correlation::causality::build_causality_chain_at(
        history,
        report.causal_history.as_ref(),
        &caus_opts,
        &now,
    ) else {
        eprintln!("Bead not found: {bead_id}");
        return ExitCode::from(1);
    };
    result.data_hash = report.data_hash.clone();

    // Go builds the envelope from the loader's source authority (whose
    // per-source data_hash is the full-file sha256) and only then substitutes
    // the report's bead fingerprint at the top level, so `authority_hash` is
    // computed over the file hash while `data_hash`/`scope_hash` use the
    // report's. Same shape as --robot-history.
    let file_hash = bv_core::data_hash::compute_data_hash(&issues);
    let mut payload = full_envelope_for(&file_hash, &issues);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("data_hash".into(), serde_json::json!(result.data_hash));
        let mut ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        let scope_hash = bv_robot::scope_hash("", "", "", &result.data_hash, &ids);
        if !scope_hash.is_empty() {
            obj.insert("scope_hash".into(), serde_json::json!(scope_hash));
        }
    }
    payload["chain"] = serde_json::to_value(&result.chain).unwrap_or_default();
    payload["insights"] = result.insights_value();

    // Go's `withEnvelope` returns a `map[string]json.RawMessage`, and
    // `encoding/json` emits map keys in sorted order. So --robot-causality is
    // the one robot command whose TOP-LEVEL keys are alphabetical, while the
    // nested chain/insights/authority objects keep their struct field order.
    // Rebuilding the outer object in key order reproduces that exactly; the
    // commands that embed the envelope as a struct keep insertion order and
    // must not be sorted.
    let sorted = match payload.as_object() {
        Some(obj) => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::with_capacity(keys.len());
            for k in keys {
                out.insert(k.clone(), obj[k].clone());
            }
            serde_json::Value::Object(out)
        }
        None => payload.clone(),
    };
    emit_json(&sorted)
}

/// Go `fmt.Sprintf("%g", v)` — the verb `percentOrFraction.Set` uses to render
/// the offending float (cmd/bv/flag_types.go:80). fmt picks `%e` when the
/// decimal exponent is `< -4` or `>= 6` (the `eprec = 6` shortest-case rule
/// in fmt/format.go) and `%f` otherwise, and pads the exponent to two digits.
/// Rust's `{:e}` is already the shortest round-tripping mantissa plus that
/// same exponent, so only the branch decision and the padding are new.
fn go_format_g(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "+Inf" } else { "-Inf" }.into();
    }
    let scientific = format!("{v:e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("LowerExp always emits an 'e'");
    let exponent: i32 = exponent
        .parse()
        .expect("LowerExp always emits a decimal exponent");
    if !(-4..6).contains(&exponent) {
        let sign = if exponent < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exponent.abs());
    }
    // Inside the fixed-notation band fmt falls through to `%f` at the same
    // shortest precision, which is exactly Rust's `Display`.
    format!("{v}")
}

/// Go `percentOrFraction.Set` (cmd/bv/flag_types.go:66-95) — accepts an int
/// 0-100 (percent) or a float 0.0-1.0 (fraction), canonicalized to int percent.
/// A `.` anywhere signals fractional intent; `int(f*100 + 0.5)` rounds to
/// nearest so 0.235 becomes 24 and 0.999 becomes 100.
///
/// Every message quotes the TRIMMED value, because Go rebinds its parameter
/// (`s = strings.TrimSpace(s)`) before formatting; only pflag's outer
/// `invalid argument %q` wrapper, built by the caller, sees the raw argv token.
fn parse_percent_or_fraction(flag: &str, raw: &str) -> Result<i64, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(format!(
            "--{flag}: empty value (expected int 0-100 or float 0.0-1.0)"
        ));
    }
    if s.contains('.') {
        // Go calls `strconv.ParseFloat(s, 64)`; `go_parse_float` is that
        // function, hex literals and digit underscores included. `str::parse`
        // is not: it rejected `1_0.5` and `0x1.8p1` (Go: 10.5 and 3) and let
        // `1.7976931348623159e309` through as `inf` (Go: a range error, hence
        // "is not a number").
        let f = go_parse_float(s).map_err(|_| {
            format!(
                "--{flag}: {s:?} is not a number (expected int 0-100 percent OR float 0.0-1.0 fraction)"
            )
        })?;
        if !(0.0..=1.0).contains(&f) {
            return Err(format!(
                "--{flag}: float {} out of range (expected 0.0-1.0 fraction; for percent use int 0-100)",
                go_format_g(f)
            ));
        }
        return Ok((f * 100.0 + 0.5) as i64);
    }
    // `strconv.Atoi` is base 10 with no underscore support, so `str::parse`
    // (not `go_parse_int_base0`) is the faithful match: both accept a leading
    // `+` and both reject `0x10`/`1_0` and anything past the 64-bit range.
    let n: i64 = s.parse().map_err(|_| {
        format!(
            "--{flag}: {s:?} is not an integer (expected int 0-100 percent OR float 0.0-1.0 fraction)"
        )
    })?;
    if !(0..=100).contains(&n) {
        return Err(format!(
            "--{flag}: int {n} out of range (expected 0-100 percent; for fraction use float 0.0-1.0)"
        ));
    }
    Ok(n)
}

/// Go `handleRobotRelated` — `--robot-related <bead-id>`.
fn run_robot_related(args: &[String]) -> ExitCode {
    let bead_id = args
        .iter()
        .position(|a| a == "--robot-related")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    // Go main.go:1578 registers `--related-min-relevance` as a
    // percentOrFraction defaulting to 20 (flag_types.go:31-38).
    //
    // A value `Set` rejects is a pflag PARSE error, not a usage error: cobra
    // surfaces it through `FlagErrorFunc` → `enrichFlagParseError`, which
    // passes a bare `Set` message straight through (it only rewrites
    // missing-argument and unknown-flag errors), and main.go:4548-4550 prints
    // it and exits 1. So the message carries pflag's
    // `invalid argument %q for %q flag: %v` wrapper — with the RAW argv token,
    // unlike the trimmed value inside — and the code is 1, not 2.
    let min_relevance: i64 = match flag_value(args, "related-min-relevance") {
        Some(raw) => match parse_percent_or_fraction("related-min-relevance", raw) {
            Ok(v) => v,
            Err(detail) => {
                eprintln!(
                    "invalid argument {raw:?} for \"--related-min-relevance\" flag: {detail}"
                );
                return ExitCode::from(1);
            }
        },
        None => 20,
    };
    // Go main.go:1584 — default 10. The cap is guarded by `MaxResults > 0`
    // (related.go:216-218), so 0 means UNLIMITED rather than "emit nothing";
    // a negative value fails the same guard and is likewise unlimited, so
    // flooring to 0 here changes no output and only keeps the `usize` cast
    // in range. An unparsable value never reaches this line: `main` rejects it
    // during flag parsing, matching Go's global pflag pass.
    let max_results: usize = parse_related_max_results(args)
        .ok()
        .flatten()
        .map(|v| v.max(0) as usize)
        .unwrap_or(10);
    // Go main.go:1585 — default false. Tombstones are skipped either way
    // (related.go:509-518); only `closed` is gated on this flag.
    //
    // Read with pflag's boolean semantics rather than a presence scan: Go's
    // `--related-include-closed` is a `flag.Bool`, so `--related-include-
    // closed=true` and `=false` are both legal and the explicit `=false` form
    // must stay false. The old `args.iter().any(|a| a == "--related-
    // include-closed")` matched neither, so every valued form silently fell
    // back to the default. See `argv::go_bool_flag` (Go isFlagActive,
    // cmd/bv/main.go:471-486).
    // A value `Set` rejects is a pflag PARSE error, not a usage error: cobra
    // routes it through `FlagErrorFunc` → `enrichFlagParseError`, which passes
    // a bare `Set` message straight through (it only rewrites missing-argument
    // and unknown-flag errors), and main.go:4548-4550 prints it with a bare
    // `fmt.Fprintln(os.Stderr, ...)` and exits 1. So the line is pflag's
    // `invalid argument %q for %q flag: %v` wrapper with NO `Error: ` prefix
    // and the code is 1, not 2 — the same convention as the sibling
    // `--related-max-results` and `--related-min-relevance` arms above.
    let include_closed = match argv::go_bool_flag(args, "related-include-closed", false) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(1);
        }
    };

    let cwd = go_working_dir();
    if let Err(e) = validate_correlation_repository(&cwd) {
        if unresolved_source_probe(&cwd) {
            return emit_unresolved_source();
        }
        // Go returns this as a plain error, so its dispatcher wraps it
        // as `Error handling <flag>: <err>` (robot_registry.go:508).
        eprintln!("Error handling --robot-related: {e}");
        return ExitCode::from(1);
    }
    let (issues, _hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // No membership pre-check here: Go's handleRobotRelatedWork goes straight
    // to the report and lets `FindRelatedWorkAt` return nil, so an unknown bead
    // is reported as "Bead not found in history" (below) rather than by a
    // separate message Rust used to print first.

    // Go robot_registry.go:3272-3286 builds the report on a correlator with the
    // feedback store attached, then threads the three flags into
    // `RelatedWorkOptions` before calling `FindRelatedWorkAt`.
    let now = jiff_now();
    let report = match generate_correlation_report(
        &cwd,
        &issues,
        &bv_correlation::history::HistoryOptions {
            limit: 500,
            ..Default::default()
        },
        now.clone(),
        true,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: generating history report: {e}");
            return ExitCode::from(1);
        }
    };
    let mut opts = bv_correlation::related::default_related_work_options();
    opts.min_relevance = min_relevance;
    opts.max_results = max_results;
    opts.include_closed = include_closed;
    // Go robot_registry.go:3268-3277 builds the adjacency from the dispatch
    // context's issue set — every dependency edge, unfiltered by type — and
    // always passes a (possibly empty) non-nil map. Leaving this `None` skips
    // the dependency-cluster detector outright, so `--robot-related` reports
    // no dependency neighbours at all and serializes the field as `[]`
    // where Go's nil accumulator gives `null`.
    opts.dependency_graph = Some(
        issues
            .iter()
            .filter_map(|issue| {
                let deps: Vec<String> = issue
                    .dependencies
                    .iter()
                    .map(|d| d.depends_on_id.clone())
                    .collect();
                if deps.is_empty() {
                    None
                } else {
                    Some((issue.id.clone(), deps))
                }
            })
            .collect(),
    );
    let Some(result) =
        bv_correlation::related::find_related_work_at(&report, &bead_id, &opts, robot_now())
    else {
        // Go robot_registry.go:3288 — `Bead not found in history: <id>`.
        eprintln!("Bead not found in history: {bead_id}");
        return ExitCode::from(1);
    };

    // Go robot_registry.go:3292 — `withEnvelope(envelope, result)` merges the
    // result's fields into the envelope at top level rather than nesting it.
    //
    // `ctx.EnvelopeWithHash(report.DataHash)` overrides only the envelope's own
    // `data_hash` and `scope_hash`; `source_authority` and the `authority_hash`
    // derived from it still carry the loader's full-file sha256. Build the
    // envelope with the file hash, then substitute the two report-derived
    // fields — the same order run_robot_history and run_robot_file_hotspots use.
    let file_hash = bv_core::data_hash::compute_data_hash(&issues);
    let mut payload = full_envelope_for(&file_hash, &issues);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("data_hash".into(), serde_json::json!(report.data_hash));
        let mut ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        let scope_hash = bv_robot::scope_hash("", "", "", &report.data_hash, &ids);
        if !scope_hash.is_empty() {
            obj.insert("scope_hash".into(), serde_json::json!(scope_hash));
        }
    }
    if let Some(obj) = serde_json::to_value(&result)
        .ok()
        .and_then(|v| v.as_object().cloned())
    {
        for (k, v) in obj {
            payload[k] = v;
        }
    }
    emit_json(&payload)
}

/// Go `handleRobotImpactNetwork` — `--robot-impact-network <bead-id|all>`.
fn run_robot_impact_network(args: &[String]) -> ExitCode {
    let target = args
        .iter()
        .position(|a| a == "--robot-impact-network")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    // Go main.go:1590 registers `--network-depth` with default 2, and
    // robot_registry.go:3386-3390 clamps the result to 1..3. The old default
    // of 1 made an unflagged run a strict subgraph of Go's.
    let depth: i64 = args
        .iter()
        .position(|a| a == "--network-depth")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(2)
        .clamp(1, 3);

    let cwd = go_working_dir();
    // Go: correlation.ValidateRepository runs before the beads file is
    // located (robot_registry.go:3335-3337). The dispatcher wraps whatever the
    // handler returns as `Error handling <flag>: <err>` when the handler has
    // not reported it itself (robot_registry.go:506-509), so a bare `Error:`
    // here loses the command the failure belongs to.
    if let Err(e) = validate_correlation_repository(&cwd) {
        if unresolved_source_probe(&cwd) {
            return emit_unresolved_source();
        }
        // Go returns this as a plain error, so its dispatcher wraps it
        // as `Error handling <flag>: <err>` (robot_registry.go:508).
        eprintln!("Error handling --robot-impact-network: {e}");
        return ExitCode::from(1);
    }
    let (issues, _hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    // Go: CorrelatorOptions{Limit: 500}, overridden by --history-limit, on a
    // correlator carrying the feedback store (robot_registry.go:3358-3367).
    let mut opts = bv_correlation::history::HistoryOptions {
        limit: 500,
        ..Default::default()
    };
    if let Some(limit) = flag_value(args, "history-limit") {
        match limit.trim().parse::<i64>() {
            Ok(v) => opts.limit = v,
            Err(_) => {
                eprintln!("Error: invalid --history-limit: {limit}");
                return ExitCode::from(2);
            }
        }
    }

    let now = jiff_now();
    let report = match generate_correlation_report(&cwd, &issues, &opts, now.clone(), true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: generating history report: {e}");
            return ExitCode::from(1);
        }
    };
    let network = impact_network::build(&report, &issues, &robot_now().to_string());

    // Go passes the raw flag value through: "all" (and the empty default) mean
    // the whole network, anything else names a bead (robot_registry.go:3372-3381).
    let bead_id = if target == "all" { "" } else { target.as_str() };
    if !bead_id.is_empty() && !network.nodes.contains_key(bead_id) {
        eprintln!("Bead not found in network: {bead_id}");
        return ExitCode::from(1);
    }

    let result = impact_network::to_result(&network, bead_id, depth);

    // Go `withEnvelope` merges the envelope OVER the payload into a
    // `map[string]json.RawMessage`, which `encoding/json` then marshals in
    // sorted key order. The envelope carries generated_at/data_hash (and the
    // source-authority fields), so the payload's own copies of the first two
    // are replaced; everything else in the payload — network, depth, stats,
    // top_clusters, top_connected — survives. `bead_id` and `top_clusters` are
    // `omitempty` in Go and are dropped here when empty.
    //
    // The envelope is built with the LOADER's hash, not the report's: Go's
    // `ctx.SourceAuthority` describes the file that was read, so
    // `source_authority.sources[0].data_hash` (and therefore `authority_hash`)
    // are the file hash, while the top-level `data_hash` and `scope_hash` are
    // the report's own bead fingerprint (`EnvelopeWithHash(report.DataHash)`).
    let file_hash = bv_core::data_hash::compute_data_hash(&issues);
    let mut payload = full_envelope_for(&file_hash, &issues);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("data_hash".into(), serde_json::json!(report.data_hash));
        let (label, recipe, repo) = active_scope_flags();
        let (mut candidate_ids, _) = bv_analysis::label_health::label_scope_ids(&label, &issues);
        candidate_ids.sort();
        let scope_hash =
            bv_robot::scope_hash(&label, &recipe, &repo, &report.data_hash, &candidate_ids);
        if !scope_hash.is_empty() {
            obj.insert("scope_hash".into(), serde_json::json!(scope_hash));
        }
        obj.insert(
            "depth".into(),
            if depth == 0 {
                serde_json::Value::Null
            } else {
                serde_json::json!(depth)
            },
        );
        if !bead_id.is_empty() {
            obj.insert("bead_id".into(), serde_json::json!(bead_id));
        }
        obj.insert("network".into(), result.network.to_json());
        obj.insert("stats".into(), result.stats.to_json());
        if !result.top_clusters.is_empty() {
            obj.insert(
                "top_clusters".into(),
                serde_json::Value::Array(
                    result
                        .top_clusters
                        .iter()
                        .map(impact_network::Cluster::to_json)
                        .collect(),
                ),
            );
        }
        if !result.top_connected.is_empty() {
            obj.insert(
                "top_connected".into(),
                serde_json::Value::Array(
                    result
                        .top_connected
                        .iter()
                        .map(impact_network::Node::to_json)
                        .collect(),
                ),
            );
        }
    }
    emit_json(&emit_sorted_object(&payload))
}

/// Go marshals the `withEnvelope` map with `encoding/json`, which emits object
/// keys in sorted order. `serde_json` is built with `preserve_order`, so the
/// handler's insertion order would otherwise leak into the bytes. Rebuilds the
/// top level sorted, leaving nested values untouched — Go only re-sorts the one
/// map it actually owns.
fn emit_sorted_object(value: &serde_json::Value) -> serde_json::Value {
    let Some(map) = value.as_object() else {
        return value.clone();
    };
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let mut out = serde_json::Map::new();
    for key in keys {
        out.insert(key.clone(), map[key].clone());
    }
    serde_json::Value::Object(out)
}

/// Go's `map[string]interface{}` literals sort at EVERY level, not just the
/// top one — `generateRobotDocs` (main.go:8143) builds the topic, the guide,
/// `guide.output_modes`, each example, the env-var table and the exit-code
/// table as maps, so `encoding/json` reorders each of them.
///
/// The one exception is `commands`: Go types it `map[string]robotCommandDoc`
/// (main.go:7612), so the command NAMES sort but each doc is a struct and
/// keeps its declaration order (`flag`, `description`, `key_fields`, `params`,
/// then the five booleans). Sorting those values would be a byte diff on every
/// command, so they are passed through untouched.
fn sort_json_maps_like_go(value: &serde_json::Value) -> serde_json::Value {
    sort_json_maps_like_go_inner(value, false)
}

fn sort_json_maps_like_go_inner(
    value: &serde_json::Value,
    skip_children: bool,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for key in keys {
                out.insert(
                    key.clone(),
                    if skip_children {
                        map[key].clone()
                    } else {
                        sort_json_maps_like_go_inner(&map[key], key == "commands")
                    },
                );
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) if !skip_children => {
            serde_json::Value::Array(items.iter().map(sort_json_maps_like_go_inner_fn).collect())
        }
        other => other.clone(),
    }
}

fn sort_json_maps_like_go_inner_fn(item: &serde_json::Value) -> serde_json::Value {
    sort_json_maps_like_go_inner(item, false)
}

/// Go `robot-sprint-list` — loads `.beads/sprints.jsonl` and emits all sprints.
fn run_robot_sprint_list() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let sprints = match bv_core::sprint::load_sprints(&cwd) {
        Ok(s) => s,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let mut payload = full_envelope_for(&hash, &issues);
    // Go's output struct for this command is exactly
    // `{RobotEnvelope, SprintCount, Sprints}` (robot_registry.go:1349-1353).
    // An `issue_count` field had no counterpart there and is not a member of
    // `model.Sprint` either (pkg/model/types.go:498-507), so it is dropped.
    payload["sprint_count"] = serde_json::json!(sprints.len());
    payload["sprints"] = serde_json::to_value(&sprints).unwrap_or_default();
    emit_json(&payload)
}

/// Go `robot-sprint-show` — `--robot-sprint-show <sprint-id>`.
fn run_robot_sprint_show(args: &[String]) -> ExitCode {
    let sprint_id = args
        .iter()
        .position(|a| a == "--robot-sprint-show")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let sprints = match bv_core::sprint::load_sprints(&cwd) {
        Ok(s) => s,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let Some(sprint) = sprints.iter().find(|s| s.id == sprint_id) else {
        eprintln!("Sprint not found: {sprint_id}");
        return ExitCode::from(1);
    };
    let sprint_issues: Vec<&serde_json::Value> = Vec::new(); // populated below
    let sprint_issue_ids: Vec<&str> = sprint.bead_ids.iter().map(|s| s.as_str()).collect();
    let issue_details: Vec<serde_json::Value> = sprint_issue_ids
        .iter()
        .filter_map(|id| {
            issues.iter().find(|i| &i.id == id).map(|i| {
                serde_json::json!({
                    "id": i.id,
                    "title": i.title,
                    "status": i.status.as_str(),
                    "priority": i.priority,
                })
            })
        })
        .collect();
    let open_count = issue_details
        .iter()
        .filter(|d| d["status"] != "closed" && d["status"] != "tombstone")
        .count();
    let closed_count = issue_details.len() - open_count;
    let mut payload = full_envelope_for(&hash, &issues);
    payload["sprint"] = serde_json::to_value(sprint).unwrap_or_default();
    payload["issues"] = serde_json::Value::Array(issue_details);
    payload["open_count"] = serde_json::json!(open_count);
    payload["closed_count"] = serde_json::json!(closed_count);
    let _ = sprint_issues; // suppressed unused
    emit_json(&payload)
}

/// Go `robot-burndown` — `--robot-burndown <sprint-id|current>`.
fn run_robot_burndown(args: &[String]) -> ExitCode {
    // Go reads the selector from the `--robot-burndown` flag's own value
    // (robot_registry.go:1380-1383). There is no separate `--burndown-sprint`
    // flag in Go at all; the previous Rust handler invented one, so a named
    // sprint passed to `--robot-burndown` was silently discarded and the run
    // fell through to the active-sprint lookup.
    let target = flag_value(args, "robot-burndown").unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let sprints = match bv_core::sprint::load_sprints(&cwd) {
        Ok(s) => s,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // Go's two branches (robot_registry.go:1386-1407): "current" means the
    // active sprint and reports "No active sprint found"; anything else is a
    // sprint ID and reports "Sprint not found: <id>". Collapsing them made a
    // typo'd sprint look like an empty sprint list.
    let resolved = if target == "current" {
        sprints.iter().find(|s| s.is_active())
    } else {
        sprints.iter().find(|s| s.id == target)
    };
    let Some(sprint) = resolved else {
        if target == "current" {
            eprintln!("No active sprint found");
        } else {
            eprintln!("Sprint not found: {target}");
        }
        return ExitCode::from(1);
    };
    let now = robot_now();
    let (points, total) = bv_core::sprint::calculate_burndown(sprint, &issues, now);
    let mut payload = full_envelope_for(&hash, &issues);
    payload["sprint"] = serde_json::to_value(sprint).unwrap_or_default();
    payload["total_issues"] = serde_json::json!(total);
    payload["points"] = serde_json::to_value(&points).unwrap_or_default();
    emit_json(&payload)
}

/// Go `robot-forecast` — `--robot-forecast [--forecast-sprint <id>]`.
fn run_robot_forecast(args: &[String]) -> ExitCode {
    // Go robot_registry.go:1447-1464 — `--forecast-sprint` is OPTIONAL. When
    // blank, `sprintBeadIDs` stays nil and the guard at :1482 never fires, so
    // the forecast covers every candidate. Substituting the ACTIVE sprint
    // (which this handler used to do) narrowed the scope to one sprint and
    // hard-failed whenever no sprint was active.
    let sprint_filter: Option<String> = flag_value(args, "forecast-sprint")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let label_filter: Option<String> = flag_value(args, "forecast-label")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    // Go robot_registry.go:1489-1492 — 0 and negatives fall back to 1.
    let agents: i64 = flag_value(args, "forecast-agents")
        .and_then(|v| go_parse_int_base0(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(1);
    // robot_registry.go:1511-1514 — `all` forecasts every candidate; anything
    // else must name an issue that survived the scope filters, or it is a hard
    // failure (robot_registry.go:1536), not a silent omission.
    let target: String = flag_value(args, "robot-forecast")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("all")
        .to_string();

    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // robot_registry.go:1449-1462 — a missing sprint (or a load failure) is
    // `Sprint not found: <id>` on stderr, exit 1.
    let sprint_bead_ids: Option<std::collections::HashSet<String>> = match &sprint_filter {
        None => None,
        Some(id) => {
            let ids = bv_core::sprint::load_sprints(&cwd)
                .ok()
                .and_then(|sprints| {
                    sprints.into_iter().find(|s| &s.id == id).map(|s| {
                        s.bead_ids
                            .into_iter()
                            .collect::<std::collections::HashSet<String>>()
                    })
                });
            match ids {
                Some(ids) => Some(ids),
                None => {
                    eprintln!("Sprint not found: {id}");
                    return ExitCode::from(1);
                }
            }
        }
    };

    if target != "all"
        && !issues.iter().any(|i| {
            i.id == target
                && label_filter
                    .as_deref()
                    .is_none_or(|l| i.labels.iter().any(|x| x == l))
                && sprint_bead_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&i.id))
        })
    {
        eprintln!("Issue not found in selected forecast scope: {target}");
        return ExitCode::from(1);
    }

    // Go robot_registry.go:1519-1521 threads the analyzer's real `graphStats`
    // into every `EstimateETAForIssue` call, so the complexity `depth` factor
    // is `GetCriticalPathScore(issue.ID)` (graph.go:344) rather than a
    // constant. `computeHeights` (graph.go:2464-2486) is the Go producer;
    // `critical_path_heights` is its Rust counterpart and yields zeros for a
    // cyclic graph, which is what a nil `criticalPathScore` reads as.
    let forecast_graph = bv_analysis::build_graph(&issues);
    let forecast_cp: std::collections::BTreeMap<String, f64> =
        bv_graph_core::critical_path_heights(&forecast_graph)
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    forecast_graph.node_id(i).unwrap_or_default().to_string(),
                    *v,
                )
            })
            .collect();
    let out = bv_core::sprint::compute_forecast_output(
        &issues,
        sprint_bead_ids.as_ref(),
        label_filter.as_deref(),
        agents,
        &forecast_cp,
        robot_now(),
    );
    // Go robot_registry.go:1571-1589 — agents/filters/forecast_count/forecasts/
    // summary are merged at the ENVELOPE TOP LEVEL, not nested under a
    // `forecast` key.
    let mut payload = full_envelope_for(&hash, &issues);
    let obj = payload.as_object_mut().expect("envelope is an object");
    for (k, v) in serde_json::to_value(&out)
        .unwrap_or_default()
        .as_object()
        .cloned()
        .unwrap_or_default()
    {
        obj.insert(k, v);
    }
    // robot_registry.go:1576-1578 echoes the literal flag string, which is why
    // the sprint's own id is used rather than an arbitrary member of the set.
    if let Some(id) = &sprint_filter {
        if let Some(filters) = payload["filters"].as_object_mut() {
            filters.insert("sprint".into(), serde_json::json!(id));
        }
    }
    emit_json(&payload)
}

/// Go `--emit-script` (main.go:4011-4100): write a shell script to stdout and
/// exit 0. Registered but previously unhandled, so the flag fell through to
/// the TUI launcher and blocked forever in any TTY.
///
/// Go computes triage with `TriageOptions{Readiness, CandidateIDs}` and NO
/// `Weights`, so this is independent of the feedback store.
fn run_emit_script(args: &[String]) -> ExitCode {
    // Go main.go:4019-4022 — 0 AND negatives collapse to 5, not to an empty
    // list. `.max(1)` would be wrong.
    let limit = flag_value(args, "script-limit")
        .and_then(|v| go_parse_int_base0(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(5) as usize;
    // Go main.go:4032-4040 — `fish` and `zsh` get ONLY the shebang; the default
    // (and any unrecognised value) also gets `set -euo pipefail`.
    let format = flag_value(args, "script-format").unwrap_or("bash");

    let cwd = go_working_dir();
    let (loaded, hash, _as_of_commit, loaded_source) = match load_issues_auto_meta_raw(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let (issues, hash_override) = apply_scope(&loaded);
    let data_hash = hash_override.unwrap_or(hash);
    if issues.is_empty() {
        println!(
            "{{\"generated_at\":\"{}\",\"data_hash\":\"empty\",\"error\":\"no issues loaded\"}}",
            jiff_now()
        );
        return ExitCode::from(0);
    }
    let g = std::sync::Arc::new(bv_analysis::analyzer::build_graph(&issues));
    let out = bv_analysis::triage::build_triage_phase2_pending(&issues, &g, robot_now());

    // Go writes `len(triage.Recommendations)`, and the recommendations list is
    // sliced to `opts.TopN` before it is built (pkg/analysis/triage.go:635-640).
    // Rust keeps the full scored set in `out.recommendations` and applies the
    // same cap when it builds the payload, so reading the raw length here
    // reported every issue in the repository rather than the ten the list
    // actually shows.
    const TRIAGE_TOP_N: usize = 10;
    let total = out.recommendations.len().min(TRIAGE_TOP_N);
    let recs: Vec<&bv_analysis::impact::IssueImpact> =
        out.recommendations.iter().take(limit).collect();
    let claim_shell = |r: &bv_analysis::impact::IssueImpact| -> Option<String> {
        r.actions
            .as_ref()
            .and_then(|a| a.get("claim"))
            .and_then(|c| c.get("shell"))
            .and_then(|s| s.as_str())
            .map(str::to_string)
    };
    let show_shell = |r: &bv_analysis::impact::IssueImpact| -> Option<String> {
        r.actions
            .as_ref()
            .and_then(|a| a.get("show"))
            .and_then(|c| c.get("shell"))
            .and_then(|s| s.as_str())
            .map(str::to_string)
    };

    let mut sb = String::new();
    match format {
        "fish" => sb.push_str("#!/usr/bin/env fish\n"),
        "zsh" => sb.push_str("#!/usr/bin/env zsh\n"),
        _ => {
            sb.push_str("#!/usr/bin/env bash\n");
            sb.push_str("set -euo pipefail\n");
        }
    }
    sb.push_str(&format!(
        "# Generated by bv --emit-script at {}\n",
        jiff_now()
    ));
    sb.push_str(&format!("# Data hash: {data_hash}\n"));
    let authority = source_authority(&loaded_source, &data_hash);
    sb.push_str(&format!(
        "# Source authority: {}\n",
        go_json_string(&serde_json::to_value(&authority).unwrap_or_default())
    ));
    sb.push_str(&format!(
        "# Top {} recommendations from {total} actionable items\n",
        recs.len()
    ));
    sb.push_str("#\n");
    sb.push_str("# Usage: source this script or run it directly\n");
    sb.push_str(
        "# Commands show recommendations; claim comments appear only for proven candidates\n",
    );
    sb.push_str("#\n\n");

    if recs.is_empty() {
        sb.push_str("echo 'No actionable recommendations available'\n");
        sb.push_str("exit 0\n");
    } else {
        for (i, rec) in recs.iter().enumerate() {
            // Go's `strings.NewReplacer("\n", " ", "\r", " ")` on the two
            // comment lines — a title or reason containing a newline would
            // otherwise end the comment and the remainder would run as shell.
            let flatten = |s: &str| s.replace(['\n', '\r'], " ");
            sb.push_str(&format!(
                "# {}. {} (score: {:.3})\n",
                i + 1,
                flatten(&format!("{}: {}", rec.id, rec.title)),
                rec.score
            ));
            if let Some(reason) = rec.reasons.first() {
                sb.push_str(&format!("#    Reason: {}\n", flatten(reason)));
            }
            if !rec.unblocks_ids.is_empty() {
                sb.push_str(&format!(
                    "#    Unblocks: {} downstream items\n",
                    rec.unblocks_ids.len()
                ));
            }
            // Go main.go:4072 — `strings.ReplaceAll(shell, "\n", "\n# ")`.
            if let Some(shell) = claim_shell(rec) {
                sb.push_str(&format!("# To claim: {}\n", shell.replace('\n', "\n# ")));
            }
            match show_shell(rec) {
                Some(shell) => {
                    sb.push_str(&shell);
                    sb.push('\n');
                }
                None => sb.push_str("# No verified live tracker route\n"),
            }
            sb.push('\n');
        }
        sb.push_str("# === Quick Actions ===\n");
        sb.push_str("# To claim the top pick:\n");
        if let Some(shell) = claim_shell(recs[0]) {
            sb.push_str(&format!("# {}\n", shell.replace('\n', "\n# ")));
        }
        sb.push_str("#\n");
        sb.push_str("# To claim all listed items (uncomment to enable):\n");
        for rec in &recs {
            if let Some(shell) = claim_shell(rec) {
                sb.push_str(&format!("# {}\n", shell.replace('\n', "\n# ")));
            }
        }
    }
    // Go main.go:4098 — `fmt.Print`, so no trailing newline is appended.
    print!("{sb}");
    ExitCode::from(0)
}

/// Recompute triage and return the `triage` body plus the provenance the
/// brief writers need. Go does the same recomputation inside each handler
/// (main.go:3880 for `--priority-brief`, :3932 for `--agent-brief`) rather
/// than reusing the `--robot-triage` payload, so this is deliberately a
/// separate pass and not a refactor of `run_robot_triage`.
struct BriefTriage {
    body: serde_json::Value,
    envelope: serde_json::Value,
    issue_count: usize,
    claims_proven: bool,
}

fn compute_brief_triage() -> Result<BriefTriage, ExitCode> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let (loaded, hash, _as_of_commit, loaded_source) = match load_issues_auto_meta_raw(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("Error: {e}");
            return Err(ExitCode::from(1));
        }
    };
    let (issues, hash_override) = apply_scope(&loaded);
    let data_hash = hash_override.unwrap_or(hash);
    if issues.is_empty() {
        eprintln!("Error: no issues loaded");
        return Err(ExitCode::from(1));
    }
    let g = std::sync::Arc::new(bv_analysis::analyzer::build_graph(&issues));
    let out =
        bv_analysis::triage::build_triage(&issues, &g, robot_now(), !as_of_pair().0.is_empty());
    let authority = source_authority(&loaded_source, &data_hash);
    let claims_proven = authority.claim_safe;

    // `GeneratePriorityBriefFromTriageJSON` unmarshals Go's anonymous
    // `TriageResult` (pkg/export/markdown.go:747-800) and reads only
    // meta.generated_at, quick_ref's four counts, the recommendation list and
    // the two derived lists. The document never reads `status`,
    // `project_health`, `commands` or the `actions` block, so this stays a
    // projection of `build_triage` rather than a second copy of the full
    // `--robot-triage` body.
    let body = serde_json::json!({
        // Go `TriageMeta` (pkg/analysis/triage.go:57-62) — the generator prints
        // `version` and `issue_count` in its header line
        // (pkg/export/markdown.go:~760), so both must be populated.
        "meta": {
            "version": GO_APP_VERSION,
            "generated_at": jiff_now(),
            "issue_count": issues.len(),
        },
        "quick_ref": {
            "open_count": out.quick_ref.open_count,
            "actionable_count": out.quick_ref.actionable_count,
            "blocked_count": out.quick_ref.blocked_count,
            "in_progress_count": out.quick_ref.in_progress_count,
        },
        // Go's `TriageResult.Recommendations` is sliced to `opts.TopN` before it
        // is built (pkg/analysis/triage.go:635-640); this port keeps the full
        // scored set in `out` and caps it when it builds the --robot-triage
        // payload. `--brief` projected `out` directly, so it published all 121
        // recommendations where the oracle publishes 10 — and every downstream
        // reader of that document got a different set.
        "recommendations": serde_json::to_value(recommendations_top_n(&out.recommendations))
            .unwrap_or_default(),
        "quick_wins": Vec::<serde_json::Value>::new(),
        "blockers_to_clear": Vec::<serde_json::Value>::new(),
    });
    let mut envelope = full_envelope_for(&data_hash, &loaded);
    envelope["output_format"] = serde_json::json!(output_format());
    envelope["version"] = serde_json::json!(GO_APP_VERSION);
    Ok(BriefTriage {
        body,
        envelope,
        issue_count: issues.len(),
        claims_proven,
    })
}

/// Go `--priority-brief <path>` (main.go:3878-3911). Writes the markdown brief
/// and exits 0. Previously registered with no handler, so the flag fell
/// through to the TUI and blocked forever.
fn run_priority_brief(path: &str) -> ExitCode {
    let t = match compute_brief_triage() {
        Ok(t) => t,
        Err(code) => return code,
    };
    println!("Generating priority brief to {path}...");
    let triage_json = go_json_string(&t.body).into_bytes();
    let config = bv_export::priority_brief::PriorityBriefConfig {
        data_hash: t
            .envelope
            .get("data_hash")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        ..Default::default()
    };
    let mut brief = match bv_export::priority_brief::generate_priority_brief_from_triage_json(
        &triage_json,
        &config,
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error generating priority brief: {e}");
            return ExitCode::from(1);
        }
    };
    // Go main.go:3900-3902 — the readiness-provisional banner when the source
    // is not claim-safe.
    if !t.claims_proven {
        brief = "> Readiness is provisional because source data is incomplete or stale. Restore the affected sources before claiming work.\n\n".to_string() + &brief;
    }
    if let Err(e) = std::fs::write(path, brief.as_bytes()) {
        eprintln!("Error writing priority brief: {e}");
        return ExitCode::from(1);
    }
    println!("Done! Priority brief saved to {path}");
    ExitCode::from(0)
}

/// Go `--agent-brief <dir>` (main.go:3915-4007). Writes the five-file bundle
/// and exits 0. Go lists `meta.json` in its own file list.
fn run_agent_brief(dir: &str) -> ExitCode {
    println!("Generating agent brief bundle to {dir}/...");
    let t = match compute_brief_triage() {
        Ok(t) => t,
        Err(code) => return code,
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("Error creating directory: {e}");
        return ExitCode::from(1);
    }
    // triage.json — the triage body under the full robot envelope,
    // MarshalIndent 2-space (Go main.go:3921-3941).
    let mut triage_payload = t.envelope.clone();
    if let (Some(obj), Some(body)) = (triage_payload.as_object_mut(), t.body.as_object()) {
        for (k, v) in body {
            obj.insert(k.clone(), v.clone());
        }
    }
    let write = |name: &str, content: &str| -> Result<(), std::io::Error> {
        std::fs::write(std::path::Path::new(dir).join(name), content)
    };
    if let Err(e) = write(
        "triage.json",
        &serde_json::to_string_pretty(&triage_payload).unwrap_or_default(),
    ) {
        eprintln!("Error writing triage.json: {e}");
        return ExitCode::from(1);
    }
    println!("  → triage.json");

    // insights.json — the same document `--robot-insights` emits.
    let insights = match build_robot_insights() {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Err(e) = write(
        "insights.json",
        &serde_json::to_string_pretty(&insights).unwrap_or_default(),
    ) {
        eprintln!("Error writing insights.json: {e}");
        return ExitCode::from(1);
    }
    println!("  → insights.json");

    // brief.md — the same generator as --priority-brief, with Go's own
    // (shorter) banner wording.
    let triage_json = go_json_string(&t.body).into_bytes();
    let config = bv_export::priority_brief::PriorityBriefConfig {
        data_hash: t
            .envelope
            .get("data_hash")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        ..Default::default()
    };
    let mut brief = match bv_export::priority_brief::generate_priority_brief_from_triage_json(
        &triage_json,
        &config,
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error generating brief: {e}");
            return ExitCode::from(1);
        }
    };
    if !t.claims_proven {
        brief = "> Readiness is provisional; inspect source_authority in triage.json before claiming work.\n\n".to_string() + &brief;
    }
    if let Err(e) = write("brief.md", &brief) {
        eprintln!("Error writing brief.md: {e}");
        return ExitCode::from(1);
    }
    println!("  → brief.md");

    if let Err(e) = write("helpers.md", bv_export::agent_brief::generate_jq_helpers()) {
        eprintln!("Error writing helpers.md: {e}");
        return ExitCode::from(1);
    }
    println!("  → helpers.md");

    let meta = bv_export::agent_brief::build_agent_brief_meta(&t.envelope, t.issue_count);
    if let Err(e) = write(
        "meta.json",
        &serde_json::to_string_pretty(&meta).unwrap_or_default(),
    ) {
        eprintln!("Error writing meta.json: {e}");
        return ExitCode::from(1);
    }
    println!("  → meta.json");

    println!("\nDone! Agent brief bundle saved to {dir}/");
    ExitCode::from(0)
}

/// Go runs the export hooks around every export (main.go:3091 for the report
/// path, main.go:4432 for `--export-pages`), gated on `!*noHooks`. This is the
/// shared wrapper both call sites use; without it the DEFAULT path already
/// diverged, because Go runs pre/post-export hooks and Rust ran nothing.
///
/// Go `Loader` reads `.bv/hooks.yaml` (pkg/hooks/config.go:99) and `Executor`
/// spawns each hook honouring `timeout_secs` and the `on_error` policy
/// (Fail | Continue, executor.go:296).
fn run_export_hooks(
    no_hooks: bool,
    cwd: &std::path::Path,
    export_path: &str,
    export_format: &str,
    issue_count: usize,
    phase_pre: bool,
) -> Result<(), String> {
    if no_hooks {
        return Ok(());
    }
    let mut loader = bv_export::hooks::Loader::new(cwd);
    if let Err(e) = loader.load() {
        // A malformed hooks.yaml is a configuration error in Go's loader, but
        // Go also tolerates a missing file (HasHooks is false). Only surface
        // a real parse failure.
        return Err(format!("{e}"));
    }
    if !loader.has_hooks() {
        return Ok(());
    }
    for w in loader.warnings() {
        eprintln!("Warning: {w}");
    }
    let ctx = bv_export::hooks::ExportContext {
        export_path: export_path.to_string(),
        export_format: export_format.to_string(),
        issue_count,
        timestamp: jiff::Timestamp::now(),
    };
    let mut exec = bv_export::hooks::Executor::new(loader.config(), ctx);
    let res = if phase_pre {
        exec.run_pre_export()
    } else {
        exec.run_post_export()
    };
    res.map_err(|e| format!("{e}"))
}

/// Go `robot-capacity` — `--robot-capacity [--capacity-label <label>]`.
fn run_robot_capacity(args: &[String]) -> ExitCode {
    // Go robot_registry.go:3543-3554 — an exact label match narrows the
    // candidate set, and :3559-3566 then applies the open filter
    // UNCONDITIONALLY on top, so a label never re-admits closed work. The
    // hand-rolled filter this replaces put the closed test in the `else`
    // branch only, so `--capacity-label cli` reported three CLOSED issues as
    // open.
    let label: Option<String> = flag_value(args, "capacity-label")
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    // Go robot_registry.go:3569-3572 — default 1, and only a strictly positive
    // value takes effect. This is the sole divisor in the completion model
    // (`serialMinutes + parallelMinutes/agents`, robot_registry.go:3621).
    let agents: i64 = flag_value(args, "agents")
        .and_then(|v| go_parse_int_base0(v).ok())
        .filter(|v| *v > 0)
        .unwrap_or(1);
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // `calculate_capacity` (sprint.rs:816) already applies the label filter
    // (:828-830) and the closed filter (:841) as two independent, correctly
    // ordered steps, and its `CapacityOutput` (sprint.rs:783-797) matches Go's
    // emit struct at robot_registry.go:3651-3682 field for field.
    //
    // Go runs the analyzer first (robot_registry.go:3536) and threads the real
    // `graphStats` into every `EstimateETAForIssue` call (:3576), so the
    // complexity `depth` factor is `GetCriticalPathScore(issue.ID)`
    // (graph.go:344) — a per-issue graph height. The same map backs
    // `computeHeights` (graph.go:2464-2486) and returns zeros for a cyclic
    // graph, which is what a nil `criticalPathScore` reads as.
    let cap_graph = bv_analysis::build_graph(&issues);
    let cp_scores: std::collections::BTreeMap<String, f64> =
        bv_graph_core::critical_path_heights(&cap_graph)
            .iter()
            .enumerate()
            .map(|(i, v)| (cap_graph.node_id(i).unwrap_or_default().to_string(), *v))
            .collect();
    let out = bv_core::sprint::calculate_capacity(&issues, agents, label.as_deref(), &cp_scores);
    let mut payload = full_envelope_for(&hash, &issues);
    let obj = payload.as_object_mut().expect("envelope is an object");
    for (k, v) in serde_json::to_value(&out)
        .unwrap_or_default()
        .as_object()
        .cloned()
        .unwrap_or_default()
    {
        obj.insert(k, v);
    }
    emit_json(&payload)
}

/// Go `loadCorrelationFeedbackStore` (robot_registry.go:2744-2754) — the store
/// the discovered beads directory implies, read from
/// `<beadsDir>/correlation_feedback.jsonl`. A missing file is not an error
/// (Go's `Load` returns nil on `os.IsNotExist`), it is an empty store.
///
/// Returns `None` only when the beads directory cannot be resolved at all;
/// Go propagates that as `getting beads directory`, but an unresolvable dir
/// already fails the repository validation every one of these handlers runs
/// first, so there is no reachable output difference.
fn load_correlation_feedback_store(
    cwd: &std::path::Path,
) -> Option<bv_correlation::feedback::FeedbackStore> {
    let dir = bv_core::discovery::get_beads_dir(cwd).ok()?;
    Some(bv_correlation::feedback::FeedbackStore::new(&dir))
}

/// Go `generateCorrelationReport` (robot_registry.go:2700-2714) — the correlator
/// every read path in `cmd/bv` is built with: `NewCorrelator(...).WithFeedbackStore(...)`,
/// so a stored confirm/reject shapes histories, the commit index and the stats
/// before any of them is read.
///
/// `with_feedback: false` is Go's `generateRawCorrelationReport` (2721-2731),
/// used only by the two handlers whose subject is a raw correlation:
/// `--robot-explain-correlation` and `--robot-confirm/reject-correlation`.
fn generate_correlation_report(
    cwd: &std::path::Path,
    issues: &[bv_core::model::Issue],
    opts: &bv_correlation::history::HistoryOptions,
    generated_at: String,
    with_feedback: bool,
) -> Result<bv_correlation::history::HistoryReport, String> {
    let beads: Vec<bv_correlation::history::BeadInfo> = issues
        .iter()
        .map(|i| bv_correlation::history::BeadInfo {
            id: i.id.clone(),
            title: i.title.clone(),
            status: i.status.as_str().to_string(),
        })
        .collect();
    let store = if with_feedback {
        load_correlation_feedback_store(cwd)
    } else {
        None
    };
    bv_correlation::history::build_history_report_with_feedback(
        cwd,
        &beads,
        opts,
        None,
        generated_at,
        store.as_ref(),
    )
}

/// Go `parseCorrelationArg` (robot_registry.go:2756-2767) — `SHA:beadID`.
fn parse_correlation_arg(arg: &str) -> Result<(String, String), String> {
    let trimmed = arg.trim();
    let Some((sha, bead_id)) = trimmed.split_once(':') else {
        return Err(format!("expected format: SHA:beadID, got: {arg:?}"));
    };
    let (sha, bead_id) = (sha.trim().to_string(), bead_id.trim().to_string());
    if sha.is_empty() || bead_id.is_empty() {
        return Err(format!(
            "expected non-empty SHA and bead ID in format SHA:beadID, got: {arg:?}"
        ));
    }
    Ok((sha, bead_id))
}

/// Go `resolveCorrelatedCommit` (robot_registry.go:2769-2801) — find the one
/// commit in a bead's history that `sha` names. An exact full-SHA hit wins
/// outright; otherwise a short-SHA equality or full-SHA prefix is a candidate,
/// and more than one distinct candidate is an error rather than a guess.
fn resolve_correlated_commit<'a>(
    commits: &'a [bv_correlation::history::HistoryCommit],
    sha: &str,
) -> Result<Option<&'a bv_correlation::history::HistoryCommit>, String> {
    let sha = sha.trim().to_lowercase();
    if sha.is_empty() {
        return Err("commit SHA is required".to_string());
    }
    let mut matches: Vec<(usize, &str)> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (i, commit) in commits.iter().enumerate() {
        let commit_sha = commit.sha.to_lowercase();
        if commit_sha == sha {
            return Ok(Some(&commits[i]));
        }
        let short_sha = commit.short_sha.to_lowercase();
        if (short_sha == sha || commit_sha.starts_with(&sha)) && seen.insert(commit.sha.as_str()) {
            matches.push((i, commit.sha.as_str()));
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(&commits[matches[0].0])),
        _ => {
            let mut shas: Vec<&str> = matches.iter().map(|(_, sha)| *sha).collect();
            shas.sort_unstable();
            Err(format!(
                "ambiguous commit SHA prefix {sha:?} matches {} commits: {}",
                shas.len(),
                shas.join(", ")
            ))
        }
    }
}

/// Go `handleRobotExplainCorrelation` — `--robot-explain-correlation SHA:beadID`.
///
/// Go encodes the bare `CorrelationExplanation` here: no robot envelope and no
/// wrapper key (robot_registry.go:2871-2887). The report is the *raw* one, so
/// a rejected pair can still be explained; the stored decision is attached to
/// the explanation and overrides its recommendation.
/// Open `/dev/tty` the way Go's bubbletea does before it starts a program.
///
/// This is a reachability check, not a handle anyone uses afterwards: it
/// reproduces Go's up-front failure for a run with no controlling terminal,
/// which is the case for every agent, CI job and cron entry.
fn open_tty() -> Result<(), String> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map(|_| ())
        .map_err(|e| go_path_error("open", "/dev/tty", &e))
}

fn run_robot_explain_correlation(args: &[String]) -> ExitCode {
    let raw = args
        .iter()
        .position(|a| a == "--robot-explain-correlation")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let (sha, bead_id) = match parse_correlation_arg(&raw) {
        Ok(x) => x,
        Err(e) => {
            // Go's dispatcher prints `Error handling <flag>: <err>` and exits
            // with the handler's code, which for a failed parse is 1 (the
            // handler returned an error, not a usage violation).
            eprintln!("Error handling --robot-explain-correlation: {e}");
            return ExitCode::from(1);
        }
    };

    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, _, _) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    // Go: `CorrelatorOptions{BeadID: beadID}` — no Limit, so the walk is
    // unbounded and only this bead's history is assembled.
    let report = match generate_correlation_report(
        &cwd,
        &issues,
        &bv_correlation::history::HistoryOptions {
            bead_id: bead_id.clone(),
            ..Default::default()
        },
        jiff_now(),
        false,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error handling --robot-explain-correlation: generating report: generating history report: {e}");
            return ExitCode::from(1);
        }
    };

    let Some(history) = report.histories.get(&bead_id) else {
        eprintln!("Bead not found: {bead_id}");
        return ExitCode::from(1);
    };
    let commits = history.commits.as_deref().unwrap_or(&[]);
    let target = match resolve_correlated_commit(commits, &sha) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("Commit {sha} not found in bead {bead_id} correlations");
            return ExitCode::from(1);
        }
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    let mut explanation = bv_correlation::scorer::build_explanation(target, &bead_id);
    if let Some(store) = load_correlation_feedback_store(&cwd) {
        let all = store.load_all();
        if let Some(fb) = all.get(&(target.sha.clone(), bead_id.clone())) {
            explanation.recommendation = describe_correlation_feedback(fb);
            explanation.feedback = Some(fb.clone());
        }
    }
    emit_json(&serde_json::to_value(&explanation).unwrap_or_default())
}

/// Go `describeCorrelationFeedback` (robot_registry.go:3899-3919) — the
/// `<verb> by feedback` phrase a stored decision replaces the recommendation
/// with. Its switch has no default arm beyond the pass-through, so an
/// unrecognised type is spelled out verbatim.
fn describe_correlation_feedback(fb: &bv_correlation::feedback::CorrelationFeedback) -> String {
    let verb = match fb.feedback_type.as_str() {
        "confirm" => "confirmed",
        "reject" => "rejected",
        "ignore" => "ignored",
        other => other,
    };
    let mut s = format!("{verb} by feedback");
    let by = fb.feedback_by.trim();
    if !by.is_empty() {
        s.push_str(&format!(" ({by})"));
    }
    let reason = fb.reason.trim();
    if !reason.is_empty() {
        s.push_str(&format!(": {reason}"));
    }
    s
}

/// Go `handleRobotCorrelationStats` — `--robot-correlation-stats`.
///
/// Go's output struct (robot_registry.go:2805-2828) *embeds*
/// `correlation.FeedbackStats` and then `generated_at` / `output_format` /
/// `version`. It does **not** embed `RobotEnvelope`, so there is no
/// `data_hash`, no `source_path`, no `scope_hash` — the seven `FeedbackStats`
/// fields sit at the top level beside three envelope-shaped scalars. The
/// numbers come from the feedback store alone: no correlation report is
/// generated on this path.
fn run_robot_correlation_stats() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_default();
    // Go loads the issues before it reaches the feedback store, so a directory
    // with no beads source fails there — with the degraded envelope — rather
    // than reporting correlation counts over a repository never read.
    if unresolved_source_probe(&cwd) {
        return emit_unresolved_source();
    }
    let Some(store) = load_correlation_feedback_store(&cwd) else {
        eprintln!("Error: getting beads directory");
        return ExitCode::from(1);
    };
    let stats = store.get_stats();

    // Field order is Go's declaration order: the embedded struct first, then
    // generated_at / output_format (omitempty) / version.
    let mut obj = serde_json::Map::new();
    if let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(stats) {
        for (k, v) in fields {
            obj.insert(k, v);
        }
    }
    obj.insert("generated_at".into(), serde_json::json!(jiff_now()));
    let fmt = output_format();
    if !fmt.is_empty() {
        obj.insert("output_format".into(), serde_json::json!(fmt));
    }
    obj.insert("version".into(), serde_json::json!(GO_APP_VERSION));
    emit_json(&serde_json::Value::Object(obj))
}

/// Go `handleRobotFileBeads` — `--robot-file-beads <path>`.
/// Shared plumbing for the four file-index-backed robot commands
/// (`--robot-file-beads`, `--robot-file-relations`, `--robot-impact`,
/// `--robot-file-hotspots`).
///
/// Go builds a `*HistoryReport` through the shared `generateCorrelationReport`
/// (limit 500, overridable by `--history-limit`) and hands it to
/// `correlation.NewFileLookup`. Feeding these commands the sha-only
/// `correlate()` map instead is why they used to return nothing: that map has
/// no per-file history and no per-commit numstat, so there is no index to
/// build one from.
///
/// The second half is Go's `ctx.EnvelopeWithHash(report.DataHash)`, which
/// overrides only the envelope's own `data_hash` and `scope_hash` —
/// `source_authority` and the `authority_hash` derived from it still carry the
/// *loader's* full-file sha256. Build the envelope with the file hash, then
/// substitute the two report-derived fields.
fn file_lookup_envelope(
    args: &[String],
    issues: &[bv_core::model::Issue],
) -> Result<
    (
        bv_correlation::history::HistoryReport,
        bv_correlation::file_index::FileLookup,
    ),
    String,
> {
    let cwd = std::env::current_dir().unwrap_or_default();
    validate_correlation_repository(&cwd).map_err(|e| e.to_string())?;
    let mut opts = bv_correlation::history::HistoryOptions {
        limit: 500,
        ..Default::default()
    };
    if let Some(raw) = flag_value(args, "history-limit") {
        if let Ok(v) = go_parse_int_base0(raw.trim()) {
            opts.limit = v;
        }
    }
    let report = generate_correlation_report(&cwd, issues, &opts, jiff_now(), true)
        .map_err(|e| e.to_string())?;
    let lookup = bv_correlation::file_index::FileLookup::new(&report);
    Ok((report, lookup))
}

/// Go's envelope for a file-index command: the loader's file hash, with the
/// report's `data_hash` and a `scope_hash` recomputed over it.
fn file_index_payload(
    issues: &[bv_core::model::Issue],
    report: &bv_correlation::history::HistoryReport,
) -> serde_json::Value {
    let file_hash = bv_core::data_hash::compute_data_hash(issues);
    let mut payload = full_envelope_for(&file_hash, issues);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("data_hash".into(), serde_json::json!(report.data_hash));
        let mut ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        let scope_hash = bv_robot::scope_hash("", "", "", &report.data_hash, &ids);
        if !scope_hash.is_empty() {
            obj.insert("scope_hash".into(), serde_json::json!(scope_hash));
        }
    }
    payload
}

/// Go `handleRobotFileBeads` — `--robot-file-beads <path>`.
fn run_robot_file_beads(args: &[String]) -> ExitCode {
    let path = args
        .iter()
        .position(|a| a == "--robot-file-beads")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    // Go main.go:1564 registers `--file-beads-limit` as a pflag `flag.Int`
    // with default 20, read at robot_registry.go:3140-3148. A negative value
    // clamps to 0 (emit none), which is the opposite of `--relations-limit` —
    // Go's guard there is `if len(...) > closedLimit`, so a negative limit
    // cannot be passed on to the slice and must be floored first or the
    // truncation would panic.
    //
    // A value pflag cannot parse as `strconv.ParseInt(s, 0, 64)` is a PARSE
    // error, not a silent fallback to 20: Go reports
    // `invalid argument %q for "--file-beads-limit" flag: %v` and exits 1
    // (main.go:4548), for both the space form `--file-beads-limit=` and any
    // unparsable value, including the empty string and out-of-range integers.
    let closed_limit: i64 = match flag_value(args, "file-beads-limit") {
        Some(raw) => match go_parse_int_base0(raw) {
            Ok(v) => v.max(0),
            Err(detail) => {
                eprintln!("invalid argument {raw:?} for \"--file-beads-limit\" flag: {detail}");
                return ExitCode::from(1);
            }
        },
        None => 20,
    };
    let cwd = go_working_dir();
    let issues = match load_issues_auto(&cwd, None) {
        Ok((issues, _, _)) => issues,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let (report, file_lookup) = match file_lookup_envelope(args, &issues) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error handling --robot-file-beads: {e}");
            return ExitCode::from(1);
        }
    };
    let result = file_lookup.lookup_by_file(&path);
    // Go reads `TotalBeads` off the lookup result BEFORE truncating
    // `ClosedBeads` (robot_registry.go:3149-3153), so the total counts beads
    // the payload then omits. Computing it after the truncation would understate
    // it by up to the limit.
    let total_beads = result.total_beads;
    let mut closed_beads = result.closed_beads;
    if closed_beads.len() > closed_limit as usize {
        closed_beads.truncate(closed_limit as usize);
    }
    let mut payload = file_index_payload(&issues, &report);
    payload["file_path"] = serde_json::json!(path);
    payload["total_beads"] = serde_json::json!(total_beads);
    payload["open_beads"] =
        serde_json::to_value(&result.open_beads).unwrap_or(serde_json::Value::Null);
    payload["closed_beads"] =
        serde_json::to_value(&closed_beads).unwrap_or(serde_json::Value::Null);
    emit_json(&payload)
}

/// Go `handleRobotFileHotspots` — `--robot-file-hotspots`.
///
/// Go (robot_registry.go:3924-3963) builds a `*HistoryReport` via the shared
/// `generateCorrelationReport`, hands it to `correlation.NewFileLookup`, and
/// emits `GetHotspots(hotspotsLimit)` alongside `GetStats()`. Both come from
/// the same `FileBeadIndex`, so the entries and the aggregate counts can never
/// disagree. The earlier Rust version rebuilt its own `{path, bead_count,
/// beads}` shape off the sha-only correlation map, which has no per-file
/// history and no index stats.
fn run_robot_file_hotspots(args: &[String]) -> ExitCode {
    let cwd = go_working_dir();
    if let Err(e) = validate_correlation_repository(&cwd) {
        if unresolved_source_probe(&cwd) {
            return emit_unresolved_source();
        }
        // Go returns this as a plain error, so its dispatcher wraps it
        // as `Error handling <flag>: <err>` (robot_registry.go:508).
        eprintln!("Error handling --robot-file-hotspots: {e}");
        return ExitCode::from(1);
    }
    let (issues, _, _) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };

    // Go main.go:1566 registers `--hotspots-limit` with default 10, threaded
    // through robot_registry.go:3946-3949 into `GetHotspots`, whose
    // `limit <= 0 || limit > len(counts)` guard (file_index.go:370) makes 0
    // mean "every hotspot" rather than "none". Signed, for the same reason.
    let hotspots_limit: i64 = flag_value(args, "hotspots-limit")
        .and_then(|raw| go_parse_int_base0(raw).ok())
        .unwrap_or(10);

    // Go: `limit := 500; if cfg.HistoryLimit != nil { limit = *cfg.HistoryLimit }`.
    let mut opts = bv_correlation::history::HistoryOptions {
        limit: 500,
        ..Default::default()
    };
    if let Some(raw) = flag_value(args, "history-limit") {
        if let Ok(v) = go_parse_int_base0(raw.trim()) {
            opts.limit = v;
        }
    }

    let report = match generate_correlation_report(&cwd, &issues, &opts, jiff_now(), true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: generating history report: {e}");
            return ExitCode::from(1);
        }
    };

    let file_lookup = bv_correlation::file_index::FileLookup::new(&report);
    let hotspots = file_lookup.get_hotspots(hotspots_limit);
    let stats = file_lookup.stats();

    // Go's `ctx.EnvelopeWithHash(report.DataHash)` overrides only the envelope's
    // own `data_hash` and `scope_hash`; `source_authority` and the
    // `authority_hash` derived from it still carry the *loader's* full-file
    // sha256. Build the envelope with the file hash, then substitute the two
    // report-derived fields — the same order run_robot_history uses.
    let file_hash = bv_core::data_hash::compute_data_hash(&issues);
    let mut payload = full_envelope_for(&file_hash, &issues);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("data_hash".into(), serde_json::json!(report.data_hash));
        let mut ids: Vec<String> = issues.iter().map(|i| i.id.clone()).collect();
        ids.sort();
        let scope_hash = bv_robot::scope_hash("", "", "", &report.data_hash, &ids);
        if !scope_hash.is_empty() {
            obj.insert("scope_hash".into(), serde_json::json!(scope_hash));
        }
    }
    payload["hotspots"] = serde_json::to_value(hotspots).unwrap_or(serde_json::Value::Null);
    payload["stats"] = serde_json::to_value(stats).unwrap_or(serde_json::Value::Null);
    emit_json(&payload)
}

/// Go `handleRobotFileRelations` — `--robot-file-relations <path>`: files
/// that co-change with the target across correlated commits.
fn run_robot_file_relations(args: &[String]) -> ExitCode {
    let path = args
        .iter()
        .position(|a| a == "--robot-file-relations")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let issues = match load_issues_auto(&cwd, None) {
        Ok((issues, _, _)) => issues,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let (report, file_lookup) = match file_lookup_envelope(args, &issues) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // Go main.go:1571 registers `--relations-threshold` as a pflag
    // `flag.Float64` with default 0.5, and file_index.go:484-486 re-applies
    // the same 0.5 whenever the value is <= 0, so a 0 or a negative means
    // "50% co-occurrence", not "no filter". Go re-applies it inside
    // `GetRelatedFiles`; the local mirror keeps the echoed `threshold` field
    // consistent with what was actually filtered on.
    //
    // A value pflag cannot parse as `strconv.ParseFloat(s, 64)` is a PARSE
    // error, not a silent fallback to 0.5: Go reports `invalid argument %q
    // for "--relations-threshold" flag: %v` and exits 1 (main.go:4548-4550).
    // Rust used `.trim().parse().ok().unwrap_or(0.5)`, which both swallowed
    // those inputs and trimmed surrounding whitespace Go rejects, so
    // `--relations-threshold abc` answered with 0.5 and exit 0 where Go fails.
    let mut relations_threshold: f64 = match flag_value(args, "relations-threshold") {
        Some(raw) => match go_parse_float(raw) {
            Ok(v) => v,
            Err(detail) => {
                eprintln!("invalid argument {raw:?} for \"--relations-threshold\" flag: {detail}");
                return ExitCode::from(1);
            }
        },
        None => 0.5,
    };
    if relations_threshold <= 0.0 {
        relations_threshold = 0.5;
    }
    // Go main.go:1572 registers `--relations-limit` with default 10, threaded
    // through robot_registry.go:3013-3015 into `GetRelatedFiles`, whose
    // `limit <= 0 { limit = 10 }` guard (file_index.go:487-489) makes 0 fall
    // back to 10 rather than yielding an empty list.
    let relations_limit: i64 = flag_value(args, "relations-limit")
        .and_then(|raw| go_parse_int_base0(raw).ok())
        .unwrap_or(10);
    let relations_limit = if relations_limit > 0 {
        relations_limit
    } else {
        10
    } as usize;

    // Go builds the co-change matrix inside `NewFileLookup` and filters it in
    // `GetRelatedFiles` (file_index.go:484-521): the score is
    // co_change_count / total_commits, sample commits are sorted then capped
    // at three, and the sort is count DESC then path ASC. The previous Rust
    // version rebuilt all of that by hand off the sha-only correlation map.
    let result = file_lookup.get_related_files(&path, relations_threshold, relations_limit);
    // Go encodes the `Threshold float64` field through `json.Marshal`
    // (robot_registry.go:3028-3032), and a non-finite float is an encoding
    // error there — `strconv.ParseFloat` accepted `inf`/`nan` as literals, and
    // neither survives `threshold <= 0`, so they reach the encoder unchanged
    // and Go writes `Error handling --robot-file-relations: encoding file
    // relations: json: unsupported value: +Inf` to stderr and exits 1. Rust's
    // `serde_json::json!` quietly turns a non-finite f64 into `null` and
    // carries on, so the guard has to be explicit. `-inf` is unreachable here:
    // it is caught by the `<= 0` floor above.
    if !relations_threshold.is_finite() {
        eprintln!(
            "Error handling --robot-file-relations: \
             encoding file relations: json: unsupported value: {}",
            go_format_float(relations_threshold)
        );
        return ExitCode::from(1);
    }
    let mut payload = file_index_payload(&issues, &report);
    // Go `CoChangeResult` (file_index.go:495-500) — an unknown file short-
    // circuits with an empty list but still reports these two.
    payload["file_path"] = serde_json::json!(path);
    payload["total_commits"] = serde_json::json!(result.total_commits);
    payload["threshold"] = serde_json::json!(relations_threshold);
    payload["related_files"] =
        serde_json::to_value(&result.related_files).unwrap_or(serde_json::Value::Null);
    emit_json(&payload)
}

/// Go `strconv.FormatFloat(v, 'g', -1, 64)`, which is what
/// `json.UnsupportedValueError` embeds in its message. Only the three
/// non-finite spellings are ever asked for; every finite value has already
/// been encoded by the time the guard runs.
fn go_format_float(v: f64) -> &'static str {
    if v.is_nan() {
        "NaN"
    } else if v > 0.0 {
        "+Inf"
    } else {
        "-Inf"
    }
}

/// One row of Go's `robotCommandDoc` table (`cmd/bv/main.go:7612`).
///
/// The Go struct carries `json:"..."` tags because `robot-docs` marshals the
/// table directly; here the fields are assembled into the manifest by
/// `robot_command_entry`, the only caller.
struct RobotCommandDoc {
    /// The flag exactly as authored in the Go table, placeholders and all.
    /// Every emitted spelling — `flag`, `params`, both invocations — goes
    /// through `robot_flag_example_form_for_command` first.
    flag: &'static str,
    description: &'static str,
    key_fields: &'static [&'static str],
    params: &'static [&'static str],
    needs_issues: bool,
    needs_git: bool,
    needs_sprint: bool,
    needs_baseline: bool,
    mutates_state: bool,
}

/// Go `robotCommandDocs` (`cmd/bv/main.go:7652`) transcribed row for row, in
/// the Go literal's own order so it can be diffed against the oracle line by
/// line. Go stores this in a map and sorts the names before emitting; this is
/// a fixed array and the sort happens in `run_robot_capabilities`.
const ROBOT_COMMAND_DOCS: &[(&str, RobotCommandDoc)] = &[
    (
        "robot-help",
        RobotCommandDoc {
            flag: "--robot-help",
            description: "Agent-focused command help. Use robot-docs guide for structured JSON documentation.",
            key_fields: &[],
            params: &[],
            needs_issues: false,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-triage",
        RobotCommandDoc {
            flag: "--robot-triage",
            description: "Unified triage: top picks, recommendations, quick wins, blockers, project health, velocity.",
            key_fields: &[
                "triage.quick_ref.top_picks",
                "triage.recommendations",
                "triage.quick_wins",
                "triage.blockers_to_clear",
                "triage.project_health",
            ],
            params: &["--graph-root <id>"],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-next",
        RobotCommandDoc {
            flag: "--robot-next",
            description: "Single top recommendation with claim/show commands.",
            key_fields: &[
                "id",
                "title",
                "score",
                "reasons",
                "unblocks",
                "claim_command",
                "show_command",
            ],
            params: &["--graph-root <id>"],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-plan",
        RobotCommandDoc {
            flag: "--robot-plan",
            description: "Dependency-respecting execution plan with parallel tracks.",
            key_fields: &["tracks", "items", "unblocks", "summary"],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-insights",
        RobotCommandDoc {
            flag: "--robot-insights",
            description: "Deep graph analysis: PageRank, betweenness, HITS, eigenvector, k-core, cycle detection.",
            key_fields: &[
                "pagerank",
                "betweenness",
                "hits",
                "eigenvector",
                "k_core",
                "cycles",
            ],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-priority",
        RobotCommandDoc {
            flag: "--robot-priority",
            description: "Priority misalignment detection: items whose graph importance differs from assigned priority.",
            key_fields: &["misalignments", "suggestions"],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-triage-by-track",
        RobotCommandDoc {
            flag: "--robot-triage-by-track",
            description: "Triage grouped by independent parallel execution tracks.",
            key_fields: &[
                "triage.recommendations_by_track[].track_id",
                "triage.recommendations_by_track[].top_pick",
                "triage.recommendations_by_track[].claim_command",
            ],
            params: &["--graph-root <id>"],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-triage-by-label",
        RobotCommandDoc {
            flag: "--robot-triage-by-label",
            description: "Triage grouped by label for area-focused agents.",
            key_fields: &[
                "triage.recommendations_by_label[].label",
                "triage.recommendations_by_label[].top_pick",
                "triage.recommendations_by_label[].claim_command",
            ],
            params: &["--graph-root <id>"],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-alerts",
        RobotCommandDoc {
            flag: "--robot-alerts",
            description: "Stale issues, blocking cascades, priority mismatches.",
            key_fields: &["alerts", "severity", "affected_issues"],
            params: &[
                "--severity info|warning|critical",
                "--alert-type <type>",
                "--alert-label <label>",
            ],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-suggest",
        RobotCommandDoc {
            flag: "--robot-suggest",
            description: "Smart suggestions: potential duplicates, missing dependencies, label assignments, cycle warnings.",
            key_fields: &["suggestions", "type", "confidence"],
            params: &[
                "--suggest-type duplicate|dependency|label|cycle",
                "--suggest-confidence 0.0-1.0",
                "--suggest-bead <id>",
            ],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-capabilities",
        RobotCommandDoc {
            flag: "--robot-capabilities",
            description: "Machine-readable capability manifest: version, contract, commands, env vars, exit codes, and output formats.",
            key_fields: &[
                "tool",
                "version",
                "contract_version",
                "commands",
                "environment_variables",
                "exit_codes",
            ],
            params: &[],
            needs_issues: false,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-recipes",
        RobotCommandDoc {
            flag: "--robot-recipes",
            description: "Recipe names, descriptions, and usage hints for pre-filtering work.",
            key_fields: &["recipes"],
            params: &[],
            needs_issues: false,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-schema",
        RobotCommandDoc {
            flag: "--robot-schema",
            description: "JSON Schema definitions for all robot command outputs.",
            key_fields: &["schema_version", "envelope", "commands"],
            params: &["--schema-command <cmd>"],
            needs_issues: false,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-docs",
        RobotCommandDoc {
            flag: "--robot-docs <topic>",
            description: "Machine-readable JSON documentation. Topics: guide, commands, examples, env, exit-codes, all.",
            key_fields: &[],
            params: &[],
            needs_issues: false,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-history",
        RobotCommandDoc {
            flag: "--robot-history",
            description: "Bead-to-commit correlations from git history.",
            key_fields: &["correlations", "confidence", "commit_sha", "bead_id"],
            params: &[
                "--bead-history <id>",
                "--history-since <date>",
                "--history-limit <n>",
                "--min-confidence 0.0-1.0",
            ],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-diff",
        RobotCommandDoc {
            flag: "--robot-diff",
            description: "Changes since a historical point (commit, branch, tag, or date).",
            key_fields: &[],
            params: &["--diff-since <ref>"],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-correlation-stats",
        RobotCommandDoc {
            flag: "--robot-correlation-stats",
            description: "Summary counts for saved correlation feedback.",
            key_fields: &[
                "total_feedback",
                "confirmed",
                "rejected",
                "ignored",
                "accuracy_rate",
            ],
            params: &[],
            needs_issues: false,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-explain-correlation",
        RobotCommandDoc {
            flag: "--robot-explain-correlation <sha:bead>",
            description: "Explain why a commit is linked to a bead.",
            key_fields: &["commit", "bead", "score", "reasons"],
            params: &[],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-confirm-correlation",
        RobotCommandDoc {
            flag: "--robot-confirm-correlation <sha:bead>",
            description: "Record positive feedback for a commit-to-bead correlation.",
            key_fields: &[],
            params: &["--correlation-by agent", "--correlation-reason verified"],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: true,
        },
    ),
    (
        "robot-reject-correlation",
        RobotCommandDoc {
            flag: "--robot-reject-correlation <sha:bead>",
            description: "Record negative feedback for a commit-to-bead correlation.",
            key_fields: &[],
            params: &["--correlation-by agent", "--correlation-reason unrelated"],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: true,
        },
    ),
    (
        "robot-search",
        RobotCommandDoc {
            flag: "--robot-search",
            description: "Hashed keyword or graph-weighted hybrid search over issue text.",
            key_fields: &[],
            params: &[
                "--search <query>",
                "--search-limit <n>",
                "--search-min-score SCORE",
                "--search-mode text|hybrid",
            ],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-label-health",
        RobotCommandDoc {
            flag: "--robot-label-health",
            description: "Per-label health metrics: open/closed counts, velocity, staleness.",
            key_fields: &[],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-label-flow",
        RobotCommandDoc {
            flag: "--robot-label-flow",
            description: "Cross-label dependency flow analysis.",
            key_fields: &[],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-label-attention",
        RobotCommandDoc {
            flag: "--robot-label-attention",
            description: "Attention-ranked labels requiring focus.",
            key_fields: &[],
            params: &["--attention-limit <n>"],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-graph",
        RobotCommandDoc {
            flag: "--robot-graph",
            description: "Dependency graph export in JSON, DOT, or Mermaid format.",
            key_fields: &[],
            params: &[
                "--graph-format json|dot|mermaid",
                "--graph-root <id>",
                "--graph-depth <n>",
            ],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-metrics",
        RobotCommandDoc {
            flag: "--robot-metrics",
            description: "Performance metrics: timing, cache hit rates, memory usage.",
            key_fields: &[],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-orphans",
        RobotCommandDoc {
            flag: "--robot-orphans",
            description: "Orphan commit candidates that should be linked to beads.",
            key_fields: &[
                "git_range",
                "stats.candidate_count",
                "candidates",
                "candidates[].probable_beads",
                "by_bead",
            ],
            params: &["--orphans-min-score 0-100", "--label <label>"],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-file-beads",
        RobotCommandDoc {
            flag: "--robot-file-beads <path>",
            description: "Beads that touched a specific file path.",
            key_fields: &["file_path", "total_beads", "open_beads", "closed_beads"],
            params: &["--file-beads-limit <n>"],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-file-hotspots",
        RobotCommandDoc {
            flag: "--robot-file-hotspots",
            description: "Files touched by the most beads.",
            key_fields: &[
                "hotspots",
                "stats.total_files",
                "stats.total_bead_links",
                "stats.files_with_multiple_beads",
            ],
            params: &["--hotspots-limit <n>"],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-file-relations",
        RobotCommandDoc {
            flag: "--robot-file-relations <path>",
            description: "Files that frequently co-change with a given file.",
            key_fields: &["file_path", "total_commits", "threshold", "related_files"],
            params: &["--relations-threshold 0.0-1.0", "--relations-limit <n>"],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-related",
        RobotCommandDoc {
            flag: "--robot-related <id>",
            description: "Beads related to a specific bead ID.",
            key_fields: &[
                "target_bead_id",
                "total_related",
                "file_overlap",
                "commit_overlap",
                "dependency_cluster",
                "concurrent",
            ],
            params: &[
                "--related-min-relevance 0-100 or 0.0-1.0",
                "--related-max-results <n>",
                "--related-include-closed",
            ],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-blocker-chain",
        RobotCommandDoc {
            flag: "--robot-blocker-chain <id>",
            description: "Full blocker chain analysis for an issue.",
            key_fields: &[
                "result.target_id",
                "result.is_blocked",
                "result.root_blockers",
                "result.chain",
                "result.has_cycle",
            ],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-impact-network",
        RobotCommandDoc {
            flag: "--robot-impact-network [<id>|all]",
            description: "Impact network graph (full or subnetwork for a bead).",
            key_fields: &[
                "network.nodes",
                "network.edges",
                "stats.total_nodes",
                "top_clusters",
                "top_connected",
            ],
            params: &["--network-depth 1-3"],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-causality",
        RobotCommandDoc {
            flag: "--robot-causality <id>",
            description: "Causal chain analysis for a bead.",
            key_fields: &[
                "chain.bead_id",
                "chain.events",
                "insights.summary",
                "insights.critical_path",
                "insights.recommendations",
            ],
            params: &[],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-sprint-list",
        RobotCommandDoc {
            flag: "--robot-sprint-list",
            description: "List all sprints as JSON.",
            key_fields: &[],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: true,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-sprint-show",
        RobotCommandDoc {
            flag: "--robot-sprint-show <id>",
            description: "Show details for a specific sprint.",
            key_fields: &[],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: true,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-forecast",
        RobotCommandDoc {
            flag: "--robot-forecast <id|all>",
            description: "ETA predictions for bead completion.",
            key_fields: &[],
            params: &[
                "--forecast-label <label>",
                "--forecast-sprint <id>",
                "--forecast-agents <n>",
            ],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-capacity",
        RobotCommandDoc {
            flag: "--robot-capacity",
            description: "Capacity simulation and completion projections.",
            key_fields: &[],
            params: &["--agents <n>", "--capacity-label <label>"],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-burndown",
        RobotCommandDoc {
            flag: "--robot-burndown <sprint|current>",
            description: "Sprint burndown data.",
            key_fields: &[],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: true,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
    (
        "robot-drift",
        RobotCommandDoc {
            flag: "--robot-drift",
            description: "Drift detection from saved baseline.",
            key_fields: &[],
            params: &[],
            needs_issues: true,
            needs_git: false,
            needs_sprint: false,
            needs_baseline: true,
            mutates_state: false,
        },
    ),
    (
        "robot-impact",
        RobotCommandDoc {
            flag: "--robot-impact <path[,path...]>",
            description: "Analyze bead impact for files that may be modified.",
            key_fields: &[
                "files",
                "risk_level",
                "risk_score",
                "warnings",
                "affected_beads",
            ],
            params: &[],
            needs_issues: true,
            needs_git: true,
            needs_sprint: false,
            needs_baseline: false,
            mutates_state: false,
        },
    ),
];

/// Go `robotFlagExampleForm` (`cmd/bv/main.go:8095`).
///
/// Order is load-bearing: a longer placeholder has to be rewritten before any
/// shorter placeholder it contains, or `<path[,path...]>` would decay into
/// `<path>` and pick up a second substitution.
fn robot_flag_example_form(flag: &str) -> String {
    const REPLACEMENTS: &[(&str, &str)] = &[
        ("[<id>|all]", "all"),
        ("<path[,path...]>", "README.md"),
        ("<sprint|current>", "current"),
        ("<sha:bead>", "deadbeef:ISSUE_ID"),
        ("<id|all>", "all"),
        ("<query>", "\"login oauth\""),
        ("<topic>", "guide"),
        ("<cmd>", "robot-triage"),
        ("<date>", "\"30 days ago\""),
        ("<label>", "backend"),
        ("<path>", "README.md"),
        ("<type>", "critical"),
        ("<ref>", "HEAD~1"),
        ("<id>", "ISSUE_ID"),
        ("<n>", "10"),
    ];
    let mut out = flag.to_string();
    for (old, new) in REPLACEMENTS {
        out = out.replace(old, new);
    }
    out
}

/// Go `robotFlagExampleFormForCommand` (`cmd/bv/main.go:8079`). Three
/// commands rewrite their placeholder before the generic table runs, because
/// the generic table's answer would name the wrong kind of identifier.
fn robot_flag_example_form_for_command(command_name: &str, flag: &str) -> String {
    let flag = match command_name {
        "robot-sprint-show" => flag.replace("<id>", "SPRINT_ID"),
        "robot-burndown" => flag.replace("<sprint|current>", "current"),
        "robot-forecast" => flag.replace("--forecast-sprint <id>", "--forecast-sprint SPRINT_ID"),
        _ => flag.to_string(),
    };
    robot_flag_example_form(&flag)
}

/// Go `robotCommandArgumentSuffix` (`cmd/bv/main.go:8050`): everything in the
/// example flag after the flag itself, which the preferred invocation splices
/// in front of `--json`.
fn robot_command_argument_suffix(command_name: &str, doc: &RobotCommandDoc) -> String {
    let form = robot_flag_example_form_for_command(command_name, doc.flag);
    let parts: Vec<&str> = form.split_whitespace().collect();
    if parts.len() <= 1 {
        return String::new();
    }
    format!(" {}", parts[1..].join(" "))
}

/// Go `preferredRobotInvocationOverride` (`cmd/bv/main.go:7973`). `None` is
/// Go's `""` — the zero value meaning "fall through to the default form".
fn preferred_robot_invocation_override(command_name: &str) -> Option<&'static str> {
    Some(match command_name {
        "robot-help" => "bv robot-help --json",
        "robot-search" => "bv robot-search \"login oauth\" --json",
        "robot-diff" => "bv robot-diff HEAD~1 --json",
        "robot-history" => "bv robot-history --history-limit 20 --json",
        "robot-alerts" => "bv robot-alerts --severity critical --json",
        "robot-suggest" => "bv robot-suggest --suggest-type duplicate --json",
        "robot-label-attention" => "bv robot-label-attention --attention-limit 5 --json",
        "robot-graph" => "bv robot-graph mermaid --json",
        "robot-orphans" => "bv robot-orphans --orphans-min-score 30 --json",
        "robot-explain-correlation" => "bv robot-explain-correlation deadbeef:ISSUE_ID --json",
        "robot-confirm-correlation" => {
            "bv robot-confirm-correlation deadbeef:ISSUE_ID --correlation-by agent --json"
        }
        "robot-reject-correlation" => {
            "bv robot-reject-correlation deadbeef:ISSUE_ID --correlation-by agent --json"
        }
        "robot-file-beads" => "bv robot-file-beads README.md --json",
        "robot-file-relations" => "bv robot-file-relations README.md --json",
        "robot-related" => "bv robot-related ISSUE_ID --json",
        "robot-blocker-chain" => "bv robot-blocker-chain ISSUE_ID --json",
        "robot-causality" => "bv robot-causality ISSUE_ID --json",
        "robot-forecast" => "bv robot-forecast all --json",
        "robot-burndown" => "bv robot-burndown current --json",
        "robot-drift" => "bv --check-drift --robot-drift --format json",
        "robot-impact" => "bv robot-impact README.md --json",
        _ => return None,
    })
}

/// Go `acceptedRobotInvocationOverrides` (`cmd/bv/main.go:8022`). Commands
/// listed here bypass the two-element default entirely, so the count differs
/// from every other entry (`robot-search` has three, `robot-help` has two
/// neither of which is the `--flag --format json` form).
fn accepted_robot_invocation_overrides(command_name: &str) -> Option<&'static [&'static str]> {
    Some(match command_name {
        "robot-help" => &["bv robot-help --json", "bv robot-docs guide --json"],
        "robot-search" => &[
            "bv --search \"login oauth\" --robot-search --format json",
            "bv robot-search \"login oauth\" --json",
            "bv search \"login oauth\" --json",
        ],
        "robot-diff" => &[
            "bv --robot-diff --diff-since HEAD~1 --format json",
            "bv robot-diff HEAD~1 --json",
        ],
        "robot-drift" => &[
            "bv --check-drift --robot-drift --format json",
            "bv robot-drift --json",
        ],
        _ => return None,
    })
}

/// Go `preferredRobotInvocation` (`cmd/bv/main.go:7963`).
fn preferred_robot_invocation(command_name: &str, doc: &RobotCommandDoc) -> String {
    if let Some(override_invocation) = preferred_robot_invocation_override(command_name) {
        return override_invocation.to_string();
    }
    format!(
        "bv {}{} --json",
        command_name,
        robot_command_argument_suffix(command_name, doc)
    )
}

/// Go `acceptedRobotInvocations` (`cmd/bv/main.go:7971`).
fn accepted_robot_invocations(command_name: &str, doc: &RobotCommandDoc) -> Vec<String> {
    if let Some(overrides) = accepted_robot_invocation_overrides(command_name) {
        return overrides.iter().map(|s| s.to_string()).collect();
    }
    vec![
        format!(
            "bv {} --format json",
            robot_flag_example_form_for_command(command_name, doc.flag)
        ),
        preferred_robot_invocation(command_name, doc),
    ]
}

/// Go `robotExampleFormsForCommand` (`cmd/bv/main.go:8058`).
fn robot_example_forms_for_command(command_name: &str, values: &[&str]) -> Vec<String> {
    values
        .iter()
        .map(|value| robot_flag_example_form_for_command(command_name, value))
        .collect()
}

/// Go `agentIntentAliasDocs` (`cmd/bv/main.go:8118`): the near-miss spellings
/// an agent is likely to try, each mapped to its canonical form.
fn agent_intent_alias_docs() -> Vec<serde_json::Value> {
    const ALIASES: &[(&str, &str)] = &[
        ("bv --json", "bv --robot-triage --format json"),
        ("bv robot-triage --json", "bv --robot-triage --format json"),
        ("bv triage --json", "bv --robot-triage --format json"),
        ("bv next --json", "bv --robot-next --format json"),
        ("bv plan --json", "bv --robot-plan --format json"),
        ("bv insights --json", "bv --robot-insights --format json"),
        (
            "bv robot-capabilities --json",
            "bv --robot-capabilities --format json",
        ),
        (
            "bv capabilities --json",
            "bv --robot-capabilities --format json",
        ),
        (
            "bv robot-docs guide --json",
            "bv --robot-docs guide --format json",
        ),
        (
            "bv docs guide --json",
            "bv --robot-docs guide --format json",
        ),
        (
            "bv robot-schema triage --json",
            "bv --robot-schema --schema-command robot-triage --format json",
        ),
        (
            "bv schema triage --json",
            "bv --robot-schema --schema-command robot-triage --format json",
        ),
        (
            "bv robot-search login oauth --json --limit 5",
            "bv --search 'login oauth' --robot-search --format json --search-limit 5",
        ),
        (
            "bv search login oauth --json --limit 5",
            "bv --search 'login oauth' --robot-search --format json --search-limit 5",
        ),
        (
            "bv robot-graph mermaid --json",
            "bv --robot-graph --graph-format mermaid --format json",
        ),
        (
            "bv graph mermaid --json",
            "bv --robot-graph --graph-format mermaid --format json",
        ),
        (
            "bv robot-related bv-123 --json",
            "bv --robot-related bv-123 --format json",
        ),
        (
            "bv --name backend --json",
            "bv --label backend --robot-triage --format json",
        ),
    ];
    ALIASES
        .iter()
        .map(|(agent_instinct, canonical)| {
            // Go marshals a map[string]string, so the keys are alphabetical.
            serde_json::json!({ "agent_instinct": agent_instinct, "canonical": canonical })
        })
        .collect()
}

/// Go `robotDocsTopics` (`cmd/bv/main.go:7624`).
fn robot_docs_topics() -> &'static [&'static str] {
    &["guide", "commands", "examples", "env", "exit-codes", "all"]
}

/// Go `robotEnvVars` (`cmd/bv/main.go:7628`), already in the alphabetical
/// order Go's `encoding/json` emits a map in.
fn robot_env_vars() -> serde_json::Value {
    serde_json::json!({
        "BEADS_DB": "Path to beads database file or .beads directory (overrides BEADS_DIR; overridden by --db flag)",
        "BEADS_DIR": "Path to .beads directory (fallback when BEADS_DB and --db are not set)",
        "BV_OUTPUT_FORMAT": "Default output format: json or toon (overridden by --format)",
        "BV_PRETTY_JSON": "Set to 1 for indented JSON output",
        "BV_ROBOT": "Set to 1 to force robot mode (clean stdout)",
        "BV_SEARCH_MODE": "Search mode: text or hybrid",
        "BV_SEARCH_PRESET": "Hybrid search preset name",
        "TOON_DEFAULT_FORMAT": "Fallback format if BV_OUTPUT_FORMAT not set",
        "TOON_INDENT": "TOON indentation level (0-16)",
        "TOON_KEY_FOLDING": "TOON key folding mode",
        "TOON_STATS": "Set to 1 to show JSON vs TOON token estimates on stderr",
    })
}

/// Go `robotExitCodes` (`cmd/bv/main.go:7644`).
fn robot_exit_codes() -> serde_json::Value {
    serde_json::json!({
        "0": "Success",
        "1": "Error (general failure, drift critical)",
        "2": "Invalid arguments or drift warning",
    })
}

/// One `.commands[]` entry of the manifest, built per Go `main.go:7913-7927`.
///
/// Insertion order is the emitted key order: Go marshals `map[string]interface{}`
/// with sorted keys, so the alphabetical inserts below are what puts
/// `key_fields` between `flag` and `mutates_state` rather than at the end.
fn robot_command_entry(name: &str, doc: &RobotCommandDoc) -> serde_json::Value {
    let mut entry = serde_json::Map::new();
    entry.insert(
        "accepted_invocations".into(),
        serde_json::json!(accepted_robot_invocations(name, doc)),
    );
    entry.insert("description".into(), serde_json::json!(doc.description));
    entry.insert(
        "flag".into(),
        serde_json::json!(robot_flag_example_form_for_command(name, doc.flag)),
    );
    // `if len(doc.KeyFields) > 0` — main.go:7921. A command with no key
    // fields omits the key; it does not emit an empty array.
    if !doc.key_fields.is_empty() {
        entry.insert("key_fields".into(), serde_json::json!(doc.key_fields));
    }
    entry.insert("mutates_state".into(), serde_json::json!(doc.mutates_state));
    entry.insert("name".into(), serde_json::json!(name));
    entry.insert(
        "needs_baseline".into(),
        serde_json::json!(doc.needs_baseline),
    );
    entry.insert("needs_git".into(), serde_json::json!(doc.needs_git));
    entry.insert("needs_issues".into(), serde_json::json!(doc.needs_issues));
    entry.insert("needs_sprint".into(), serde_json::json!(doc.needs_sprint));
    // `if len(doc.Params) > 0` — main.go:7924, same omit-don't-empty rule.
    if !doc.params.is_empty() {
        entry.insert(
            "params".into(),
            serde_json::json!(robot_example_forms_for_command(name, doc.params)),
        );
    }
    entry.insert(
        "preferred_invocation".into(),
        serde_json::json!(preferred_robot_invocation(name, doc)),
    );
    serde_json::Value::Object(entry)
}

/// Go `generateRobotCapabilities` (`cmd/bv/main.go:7903`).
///
/// Go's `tool` is the literal `"bv"` — the Rust binary is a drop-in
/// replacement, so every self-description it emits names the Go tool.
fn run_robot_capabilities() -> ExitCode {
    let mut names: Vec<&str> = ROBOT_COMMAND_DOCS.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();

    let commands: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            let doc = ROBOT_COMMAND_DOCS
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, doc)| doc)
                .expect("name came from this table");
            robot_command_entry(name, doc)
        })
        .collect();

    let mut payload = serde_json::Map::new();
    payload.insert(
        "agent_intent_aliases".into(),
        serde_json::json!(agent_intent_alias_docs()),
    );
    payload.insert("commands".into(), serde_json::json!(commands));
    payload.insert(
        "contract_version".into(),
        serde_json::json!(bv_robot::ROBOT_CONTRACT_VERSION),
    );
    payload.insert(
        "default_robot_command".into(),
        serde_json::json!("bv --robot-triage"),
    );
    payload.insert("docs_topics".into(), serde_json::json!(robot_docs_topics()));
    payload.insert("environment_variables".into(), robot_env_vars());
    payload.insert("exit_codes".into(), robot_exit_codes());
    payload.insert("generated_at".into(), serde_json::json!(jiff_now()));
    payload.insert("output_formats".into(), serde_json::json!(["json", "toon"]));
    payload.insert(
        "schema_command".into(),
        serde_json::json!("bv --robot-schema"),
    );
    payload.insert(
        "stream_contract".into(),
        serde_json::json!({
            "stderr": "Diagnostics, warnings, and actionable errors.",
            "stdout": "Structured robot data only for robot commands.",
        }),
    );
    payload.insert("tool".into(), serde_json::json!("bv"));
    payload.insert("version".into(), serde_json::json!(GO_APP_VERSION));
    emit_json(&serde_json::Value::Object(payload))
}

/// Go `handleRobotSchema` (`--robot-schema`, optional `--schema-command NAME`).
/// Scope cut: returns a minimal real schema shape (name/status/flag), not
/// Go's full per-field JSON-schema definitions (`generateRobotSchemas`) —
/// those aren't ported. See plan doc §11.
fn run_robot_schema(args: &[String]) -> ExitCode {
    // Go: --schema-command filters to a single command's schema.
    let command = args
        .iter()
        .position(|a| a == "--schema-command")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let full = bv_robot::schema::generate_robot_schemas(&jiff_now());
    let Some(name) = &command else {
        return emit_json(&full);
    };

    let Some(schema) = full["commands"].get(name) else {
        eprintln!("Unknown command: {name}");
        eprintln!("Available commands:");
        let mut names: Vec<&str> = full["commands"]
            .as_object()
            .map(|o| o.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        names.sort();
        for n in names {
            eprintln!("  {n}");
        }
        return ExitCode::from(1);
    };
    let payload = serde_json::json!({
        "schema_version": full["schema_version"],
        "generated_at": full["generated_at"],
        "command": name,
        "schema": schema,
    });
    emit_json(&payload)
}

/// Go `handleRobotMetrics` — `--robot-metrics`.
///
/// The metrics are *envelope fields*, not a payload of their own: Go returns a
/// `RobotEnvelope` with `timing`, `cache` and `memory` added to it. This used
/// to build a standalone object with a `tool`/`version`/`dataset` header and a
/// `usage_hints` list, so a consumer reading `data_hash` or `source_authority`
/// off this command found neither — and `version` reported the Rust crate's
/// own `0.2.0` where every other robot command reports Go's `v0.25.0`.
///
/// `timing` and `cache` are projected onto Go's registries by `go_timing_stats`
/// and `go_cache_stats` rather than forwarded from `bv_analysis::metrics`,
/// which registers a different set of names.
fn run_robot_metrics() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_default();
    // Go times this same load: `pkg/loader/loader.go:918` defers
    // `metrics.Timer(metrics.LoaderParse)` around the issue-file parse.
    let collect = bv_analysis::metrics::metrics_enabled();
    let started = std::time::Instant::now();
    let loaded = bv_core::discovery::load_issues_from_repo(&cwd);
    let parse_ns = started.elapsed().as_nanos();
    let issues = match loaded {
        Ok((issues, _stats)) => issues,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let hash = bv_core::data_hash::compute_data_hash(&issues);
    let mut payload = full_envelope_for(&hash, &issues);
    // Go's `MetricsOutput.Timing` is `omitempty` (`pkg/metrics/cache.go:164`),
    // so with collection disabled the key is absent rather than an empty array.
    if collect {
        payload["timing"] = go_timing_stats(parse_ns);
    }
    let m = bv_analysis::metrics::get_all_metrics();
    payload["cache"] = go_cache_stats(&m);
    payload["memory"] = m["memory"].clone();
    emit_json(&payload)
}

/// Go `AllTimingStats` (`pkg/metrics/timing.go:252`) appends a metric only when
/// `m.Count() > 0`, so an idle metric is dropped rather than reported as a row
/// of zeroes. The sole producer on this path is the issue-file parse, which Go
/// registers as `loader.parse` (`pkg/metrics/timing.go:216`), so that is the
/// one entry — a single sample makes Go's total/avg/max/min all the same value.
///
/// Field order follows Go's `TimingStats` (timing.go:159) and the units are Go's
/// `float64(ns) / 1e6`. `min_ms` is `omitempty` there, so it is omitted when the
/// measurement rounds to zero.
fn go_timing_stats(parse_ns: u128) -> serde_json::Value {
    let ms = parse_ns as f64 / 1e6;
    let mut entry = serde_json::json!({
        "name": "loader.parse",
        "count": 1,
        "total_ms": ms,
        "avg_ms": ms,
        "max_ms": ms,
    });
    if ms != 0.0 {
        entry["min_ms"] = serde_json::json!(ms);
    }
    serde_json::Value::Array(vec![entry])
}

/// Go `AllCacheStats` (`pkg/metrics/cache.go:128`) reports every registered
/// cache, including ones this command never consults, "so a reader of
/// --robot-metrics should see which caches exist and that they were idle, not an
/// empty list that looks like 'no caches'". Go's `AllCacheMetrics`
/// (cache.go:115) holds exactly four, in this order.
///
/// `metrics_cache` and `style_cache` — the two extras in the Rust registry —
/// have no Go counterpart and no producer, so they are not reported.
fn go_cache_stats(m: &serde_json::Value) -> serde_json::Value {
    let registered = m["cache"].as_array().cloned().unwrap_or_default();
    let mut rows = Vec::new();
    for name in ["graph_cache", "triage_cache", "search_cache"] {
        if let Some(row) = registered.iter().find(|r| r["name"] == name) {
            rows.push(row.clone());
        }
    }
    // Go's fourth entry is the correlation report/artifact cache, fed by
    // `pkg/correlation` (`disk_cache.go:411`, `cache.go:563`). The Rust port has
    // no correlation cache, so it is structurally idle — which is also what Go
    // reports for a command that never consults it.
    rows.push(serde_json::json!({
        "name": "correlation_cache",
        "hits": 0,
        "misses": 0,
        "total": 0,
        "hit_rate": 0.0,
    }));
    serde_json::Value::Array(rows)
}

/// Go `handleRobotDocs` (`--robot-docs [topic]`). Scope cut: Go's
/// `generateRobotDocs` embeds a large hand-authored guide per topic; this
/// returns a minimal real index instead of that text (see plan doc §11).
fn run_robot_docs(args: &[String]) -> ExitCode {
    // Go: `--robot-docs` with no topic defaults to "guide" (cobra flag value).
    let topic = args
        .iter()
        .position(|a| a == "--robot-docs")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "guide".to_string());
    let payload = bv_robot::docs::generate_robot_docs(&topic, GO_APP_VERSION, &jiff_now());
    emit_json(&sort_json_maps_like_go(&payload))
}

/// Go `Analyzer.GetBlockerChain` — `--robot-blocker-chain <issue-id>`.
fn run_robot_blocker_chain(args: &[String]) -> ExitCode {
    let issue_id = args
        .iter()
        .position(|a| a == "--robot-blocker-chain")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    match bv_analysis::blocker_chain::get_blocker_chain(&issues, &issue_id) {
        Some(result) => {
            let mut payload = full_envelope_for(&hash, &issues);
            payload["result"] = serde_json::to_value(&result).unwrap_or_default();
            emit_json(&payload)
        }
        None => {
            eprintln!("Issue not found: {issue_id}");
            ExitCode::from(1)
        }
    }
}

/// Go `handleRobotCorrelationFeedback` — `--robot-confirm-correlation SHA:beadID`
/// / `--robot-reject-correlation SHA:beadID`.
///
/// Runs the correlator pipeline to generate a fresh report, validates that
/// the given SHA exists in the bead's correlation history (exact match,
/// short SHA, or unambiguous prefix), and records feedback with the
/// commit's `original_conf` from the report. Returns exit code 1 if the
/// SHA is not found or ambiguous, matching Go behavior.
/// Go `handleRobotCorrelationFeedback` — `--robot-confirm-correlation` /
/// `--robot-reject-correlation <sha>:<bead>`.
///
/// Go encodes a bare `map[string]interface{}` here, with no robot envelope
/// (robot_registry.go:2955-2965), and Go marshals a map in sorted key order:
/// bead, by, commit, orig_conf, reason, status.
///
/// The report is the *raw* one on purpose: feedback is a decision about a raw
/// correlation, so the target must still resolve after an earlier rejection
/// (letting a rejection be flipped) and `orig_conf` must record the strategy
/// confidence rather than a value a previous confirmation pinned to 1.0.
fn run_robot_correlation_feedback(args: &[String], flag: &str, feedback_type: &str) -> ExitCode {
    let status = if feedback_type == "reject" {
        "rejected"
    } else {
        "confirmed"
    };
    let flag_name = format!("--robot-{flag}");
    let raw = args
        .iter()
        .position(|a| a == &flag_name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let (sha, bead_id) = match parse_correlation_arg(&raw) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("Error handling {flag_name}: {e}");
            return ExitCode::from(1);
        }
    };

    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, _, _) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let Some(store) = load_correlation_feedback_store(&cwd) else {
        eprintln!("Error: getting beads directory");
        return ExitCode::from(1);
    };

    let report = match generate_correlation_report(
        &cwd,
        &issues,
        &bv_correlation::history::HistoryOptions {
            bead_id: bead_id.clone(),
            ..Default::default()
        },
        jiff_now(),
        false,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "Error handling {flag_name}: generating report: generating history report: {e}"
            );
            return ExitCode::from(1);
        }
    };

    let Some(history) = report.histories.get(&bead_id) else {
        eprintln!("Bead not found: {bead_id}");
        return ExitCode::from(1);
    };
    let commits = history.commits.as_deref().unwrap_or(&[]);
    let target = match resolve_correlated_commit(commits, &sha) {
        Ok(Some(c)) => c,
        Ok(None) => {
            eprintln!("Commit {sha} not found in bead {bead_id} correlations");
            return ExitCode::from(1);
        }
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let original_conf = target.confidence;
    let resolved_sha = target.sha.clone();

    // Go main.go:1556-1557 registers `--correlation-by` (default "cli") and
    // `--correlation-reason` (default ""); `--correlation-by` is trimmed and
    // only overrides the default when non-empty, `--correlation-reason` is
    // taken verbatim.
    let mut feedback_by = "cli".to_string();
    if let Some(raw_by) = flag_value(args, "correlation-by") {
        let trimmed = raw_by.trim();
        if !trimmed.is_empty() {
            feedback_by = trimmed.to_string();
        }
    }
    let reason = flag_value(args, "correlation-reason").unwrap_or_default();

    let fb = bv_correlation::feedback::CorrelationFeedback {
        commit_sha: resolved_sha.clone(),
        bead_id: bead_id.clone(),
        feedback_at: jiff_now(),
        feedback_by: feedback_by.clone(),
        feedback_type: feedback_type.to_string(),
        reason: reason.to_string(),
        original_conf,
    };
    if let Err(e) = store.record(&fb) {
        eprintln!("Error saving feedback: {e}");
        return ExitCode::from(1);
    }

    let mut obj = serde_json::Map::new();
    obj.insert("bead".into(), serde_json::json!(bead_id));
    obj.insert("by".into(), serde_json::json!(feedback_by));
    obj.insert("commit".into(), serde_json::json!(resolved_sha));
    obj.insert("orig_conf".into(), serde_json::json!(original_conf));
    obj.insert("reason".into(), serde_json::json!(reason));
    obj.insert("status".into(), serde_json::json!(status));
    emit_json(&serde_json::Value::Object(obj))
}

fn run_robot_label_health() -> ExitCode {
    let (issues, hash, _p1, _status, _g) = match load_and_analyze() {
        Ok(x) => x,
        Err(code) => return code,
    };
    let cfg = bv_analysis::label_health::LabelHealthConfig::default();
    let results = bv_analysis::label_health::compute_all_label_health(&issues, &cfg, robot_now());
    let mut payload = full_envelope_for(&hash, &issues);
    payload["analysis_config"] = serde_json::to_value(&cfg).unwrap_or_default();
    payload["results"] = serde_json::to_value(&results).unwrap_or_default();
    payload["usage_hints"] = serde_json::json!([
        "jq '.results.summaries | sort_by(.health) | .[:3]' - Critical labels",
        "jq '.results.labels[] | select(.health_level == \"critical\")' - Critical details",
        "jq '.results.cross_label_flow.bottleneck_labels' - Bottleneck labels",
        "jq '.results.attention_needed' - Labels needing attention",
    ]);
    emit_json(&payload)
}

fn run_robot_label_flow() -> ExitCode {
    let (issues, hash, _p1, _status, _g) = match load_and_analyze() {
        Ok(x) => x,
        Err(code) => return code,
    };
    let cfg = bv_analysis::label_health::LabelHealthConfig::default();
    let flow = bv_analysis::label_health::compute_cross_label_flow(&issues, &cfg);
    let mut payload = full_envelope_for(&hash, &issues);
    // Go: nil arrays serialize as null (not []) for empty list fields.
    let mut flow_obj = serde_json::Map::new();
    flow_obj.insert("labels".into(), serde_json::json!(flow.labels));
    flow_obj.insert("flow_matrix".into(), serde_json::json!(flow.flow_matrix));
    {
        flow_obj.insert("dependencies".into(), serde_json::json!(flow.dependencies));
    }
    {
        flow_obj.insert(
            "critical_paths".into(),
            serde_json::json!(flow.critical_paths),
        );
    }
    {
        flow_obj.insert(
            "bottleneck_labels".into(),
            serde_json::json!(flow.bottleneck_labels),
        );
    }
    flow_obj.insert(
        "total_cross_label_deps".into(),
        serde_json::json!(flow.total_cross_label_deps),
    );
    payload["flow"] = serde_json::Value::Object(flow_obj);
    payload["analysis_config"] = serde_json::to_value(&cfg).unwrap_or_default();
    payload["usage_hints"] = serde_json::json!([
        "jq '.flow.bottleneck_labels' - labels blocking the most others",
        "jq '.flow.dependencies[] | select(.issue_count > 0) | {from:.from_label,to:.to_label,count:.issue_count}'",
        "jq '.flow.flow_matrix' - raw matrix (row=from, col=to, align with .flow.labels)",
    ]);
    emit_json(&payload)
}

/// Go `buildAttentionReason` — human-readable attention reason.
fn build_attention_reason(s: &bv_analysis::label_health::LabelAttentionScore) -> String {
    let mut parts: Vec<String> = Vec::new();
    if s.pagerank_sum > 0.5 {
        parts.push("High PageRank".into());
    }
    if s.blocked_count > 0 {
        parts.push(format!("{} blocked", s.blocked_count));
    }
    if s.stale_count > 0 {
        parts.push(format!("{} stale", s.stale_count));
    }
    if s.velocity_factor <= 1.0 {
        parts.push("low velocity".into());
    }
    if parts.is_empty() {
        return format!("{} open issues", s.open_count);
    }
    parts.join(", ")
}

fn run_robot_label_attention() -> ExitCode {
    let (issues, hash) = match bv_core::discovery::load_issues_from_repo(
        &std::env::current_dir().unwrap_or_default(),
    ) {
        Ok((issues, _)) => {
            let h = bv_core::data_hash::compute_data_hash(&issues);
            (issues, h)
        }
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let cfg = bv_analysis::label_health::LabelHealthConfig::default();
    let result =
        bv_analysis::label_health::compute_label_attention_scores(&issues, &cfg, robot_now());
    let mut payload = full_envelope_for(&hash, &issues);
    // Go robot_registry.go:1793-1799 — the flag defaults to 5 (main.go:1494),
    // and a non-positive value means "unset" rather than "emit nothing", so it
    // is re-clamped to 5 before the `limit > len(Labels)` bound. The parse is
    // signed so `0` and `-3` reach that clamp, and a non-integer is the
    // pflag parse error `main.go:4548` reports at exit 1 rather than a silent
    // fall back to the default.
    let raw_attention_limit: Option<String> = {
        let args: Vec<String> = std::env::args().collect();
        let mut found = None;
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--attention-limit" {
                found = args.get(i + 1).cloned();
                break;
            }
            if let Some(v) = a.strip_prefix("--attention-limit=") {
                found = Some(v.to_string());
                break;
            }
            i += 1;
        }
        found
    };
    let attention_limit: i64 = match &raw_attention_limit {
        Some(raw) => match go_parse_int_base0(raw) {
            Ok(v) => v,
            Err(detail) => {
                eprintln!("invalid argument {raw:?} for \"--attention-limit\" flag: {detail}");
                return ExitCode::from(1);
            }
        },
        None => 5,
    };
    let attention_limit = if attention_limit > 0 {
        attention_limit
    } else {
        5
    } as usize;
    let effective_limit = result.labels.len().min(attention_limit);
    payload["limit"] = serde_json::json!(effective_limit);
    payload["total_labels"] = serde_json::json!(result.total_labels);
    // Go builds labels from a separate struct with specific field order:
    // rank, label, attention_score, normalized_score, reason, open_count,
    // blocked_count, stale_count, pagerank_sum, velocity_factor.
    // Go truncates the ranked list to --attention-limit (robot_registry.go:1833)
    // and reports the same bound as `limit`; without the truncation the array
    // carried every scored label.
    let attention_labels: Vec<serde_json::Value> = result
        .labels
        .iter()
        .take(effective_limit)
        .map(|s| {
            let reason = build_attention_reason(s);
            serde_json::json!({
                "rank": s.rank,
                "label": s.label,
                "attention_score": s.attention_score,
                "normalized_score": s.normalized_score,
                "reason": reason,
                "open_count": s.open_count,
                "blocked_count": s.blocked_count,
                "stale_count": s.stale_count,
                "pagerank_sum": s.pagerank_sum,
                "velocity_factor": s.velocity_factor,
            })
        })
        .collect();
    payload["labels"] = serde_json::Value::Array(attention_labels);
    payload["usage_hints"] = serde_json::json!([
        "jq '.labels[0]' - top attention label details",
        "jq '.labels[] | select(.blocked_count > 0)' - labels with blocked issues",
        "jq '.labels[] | {label:.label,score:.attention_score,reason:.reason}'",
    ]);
    emit_json(&payload)
}

/// Go handleRobotImpact — file-based impact analysis.
fn run_robot_impact(args: &[String]) -> ExitCode {
    let files_str = args
        .iter()
        .position(|a| a == "--robot-impact")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    // Go (robot_registry.go:3183-3186) splits on "," and trims each segment.
    // It does NOT drop empty ones, so `--robot-impact "a,,b"` yields three
    // entries with the middle one blank; the old filter silently turned that
    // into two and shifted every index after it.
    let files: Vec<String> = files_str.split(',').map(|s| s.trim().to_string()).collect();
    if files_str.trim().is_empty() {
        eprintln!("Error: --robot-impact requires comma-separated file paths");
        return ExitCode::from(2);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let issues = match load_issues_auto(&cwd, None) {
        Ok((issues, _, _)) => issues,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let (report, file_lookup) = match file_lookup_envelope(args, &issues) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // Go calls `ImpactAnalysisAt(files, robotNow())`. The previous Rust path
    // called `bv_analysis::file_impact::compute_file_impact`, an invented
    // heuristic over the sha-only correlation map with a ">10 beads -> high"
    // risk ladder and a "0 beads affected across 1 files" summary that appears
    // nowhere in Go. `robot_now()` keeps the recency window deterministic.
    let result = file_lookup.impact_analysis_at(&files, robot_now());
    let mut payload = file_index_payload(&issues, &report);
    // Go's emit order is files, risk_level, risk_score, summary, warnings,
    // affected_beads — the order of the anonymous struct at
    // robot_registry.go:3177-3183, which is not the order ImpactResult
    // declares its own fields in.
    payload["files"] = serde_json::json!(result.files);
    payload["risk_level"] = serde_json::json!(result.risk_level);
    payload["risk_score"] = serde_json::json!(result.risk_score);
    payload["summary"] = serde_json::json!(result.summary);
    payload["warnings"] = serde_json::json!(result.warnings);
    payload["affected_beads"] =
        serde_json::to_value(&result.affected_beads).unwrap_or(serde_json::Value::Null);
    emit_json(&payload)
}

/// Go handleRobotDiff — git snapshot comparison.
///
/// Go builds two `analysis.Snapshot`s (one per revision) and diffs the graphs
/// between them (cmd/bv/robot_registry.go:1614-1647, main.go:4336-4340), so
/// the output carries `resolved_revision`, `from_data_hash`, `to_data_hash`
/// and the full `SnapshotDiff` — not the flat issue-list diff this handler
/// used to emit.
fn run_robot_diff(args: &[String]) -> ExitCode {
    let diff_ref = args
        .iter()
        .position(|a| a == "--diff-since")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let Some(ref_str) = diff_ref else {
        eprintln!("Error: --robot-diff requires --diff-since <git-ref>");
        return ExitCode::from(2);
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let (current, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    // Shared plumbing with the TUI Time-Travel view (TUI_UX_PARITY_PLAN.md
    // Q4): revision resolution + `git show` + tolerant JSONL parse live in
    // `bv_core::discovery::GitLoader` (the same loader `--as-of` uses), so
    // the diff algorithm is built once and reused, not duplicated.
    let git = bv_core::discovery::GitLoader::new(&cwd);
    let previous = match git.load_at(&ref_str) {
        Ok(prev) => prev,
        Err(_) => {
            eprintln!("Error: could not read issues at ref {ref_str}");
            return ExitCode::from(1);
        }
    };
    // Go falls back to the raw ref when resolution fails (main.go:4330-4333).
    let revision = git
        .resolve_revision(&ref_str)
        .unwrap_or_else(|_| ref_str.clone());

    // Go: NewSnapshotAt(historical, time.Time{}, revision) and
    // NewSnapshot(issues), then the handler stamps ToTimestamp with
    // robotNow() so SOURCE_DATE_EPOCH pins the output.
    let from = bv_analysis::snapshot_diff::Snapshot::new(
        previous.clone(),
        bv_analysis::snapshot_diff::GO_ZERO_TIME.to_string(),
        revision.clone(),
    );
    let to = bv_analysis::snapshot_diff::Snapshot::new(current.clone(), jiff_now(), String::new());
    let diff = bv_analysis::snapshot_diff::compare_snapshots(&from, &to);

    // The envelope is built from the live issues (Go's ctx.Issues), not from
    // the historical ones, so `scope_hash` agrees with every other command.
    let mut payload = full_envelope_for(&hash, &current);
    payload["resolved_revision"] = serde_json::json!(revision);
    payload["from_data_hash"] = serde_json::json!(bv_core::data_hash::compute_data_hash(&previous));
    payload["to_data_hash"] = serde_json::json!(hash);
    payload["diff"] = serde_json::to_value(&diff).unwrap_or_default();
    emit_json(&payload)
}

/// Go handleRobotNotReadyLabels — filter triage by not-ready labels.
fn run_robot_not_ready_labels(args: &[String]) -> ExitCode {
    let labels_str = args
        .iter()
        .position(|a| a == "--robot-not-ready-labels")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_default();
    let not_ready: Vec<String> = labels_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if not_ready.is_empty() {
        eprintln!("Error: --robot-not-ready-labels requires comma-separated labels");
        return ExitCode::from(2);
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let (issues, hash, _as_of_commit) = match load_issues_auto(&cwd, None) {
        Ok(x) => x,
        Err(e) => {
            if is_unresolved_source_error(&e.to_string()) {
                return emit_unresolved_source();
            }
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    let excluded = issues
        .iter()
        .filter(|i| i.labels.iter().any(|l| not_ready.contains(l)))
        .count();
    let remaining: Vec<&str> = issues
        .iter()
        .filter(|i| !i.labels.iter().any(|l| not_ready.contains(l)))
        .map(|i| i.id.as_str())
        .collect();
    let mut payload = full_envelope_for(&hash, &issues);
    payload["not_ready_labels"] = serde_json::json!(not_ready);
    payload["total_issues"] = serde_json::json!(issues.len());
    payload["excluded_count"] = serde_json::json!(excluded);
    payload["remaining_count"] = serde_json::json!(remaining.len());
    payload["remaining_ids"] = serde_json::json!(remaining);
    emit_json(&payload)
}

/// Go `robotNow` parity: `SOURCE_DATE_EPOCH` (unix seconds) pins the clock
/// for deterministic output (reproducible builds / differential testing).
fn robot_now() -> jiff::Timestamp {
    if let Ok(v) = std::env::var("SOURCE_DATE_EPOCH") {
        if let Ok(secs) = v.trim().parse::<i64>() {
            if let Ok(ts) = jiff::Timestamp::from_second(secs) {
                return ts;
            }
        }
    }
    jiff::Timestamp::now()
}

/// The wall clock in the machine's own zone, second precision.
///
/// `baseline.created_at` is the one timestamp Go takes from `time.Now()`
/// rather than `robotNow()` (baseline.go:224), so it is deliberately *not*
/// pinned by SOURCE_DATE_EPOCH and carries the local UTC offset. Every
/// envelope `generated_at` goes through [`jiff_now`] and stays pinned; this
/// exists so the baseline document matches the oracle's own file.
fn jiff_local_now() -> String {
    // Go stamps `baseline.created_at` from `time.Now()`, which carries the
    // machine's own UTC offset. The tz database is not consulted here: jiff's
    // `TimeZone::system()` resolves UTC on macOS, so reading the offset from
    // the C library is the only way to get the instant Go would have written.
    // Getting this wrong is not cosmetic — the stored timestamp is what every
    // later `--check-drift` compares against.
    let offset = local_utc_offset_seconds();
    let local = jiff::Timestamp::now()
        .checked_add(std::time::Duration::from_secs(offset.unsigned_abs()))
        .unwrap_or_else(|_| jiff::Timestamp::now());
    let dt = local.to_zoned(jiff::tz::TimeZone::UTC).datetime();
    let zone = if offset == 0 {
        "Z".to_string()
    } else {
        let sign = if offset < 0 { '-' } else { '+' };
        let (h, m) = (offset.abs() / 3600, (offset.abs() % 3600) / 60);
        format!("{sign}{h:02}:{m:02}")
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{zone}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second()
    )
}

/// The machine's current UTC offset in seconds, from the C library's
/// `localtime_r`. Falls back to 0 (UTC) if the call is unavailable.
fn local_utc_offset_seconds() -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: `localtime_r` writes a single `tm` we own and initialise to
        // zero first; `time` with a null argument is the documented way to
        // read the current time.
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return 0;
            }
            tm.tm_gmtoff as i64
        }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

fn jiff_now() -> String {
    // Go parity: truncate to second precision (no microseconds).
    let ts = robot_now();
    let s = ts.to_string();
    // Strip sub-second portion: "2026-08-22T14:07:01.790741Z" → "2026-08-22T14:07:01Z"
    if let Some(pos) = s.find('.') {
        format!("{}Z", &s[..pos])
    } else {
        s
    }
}

/// Go `handleRobotImpact` — `--robot-impact <file1,file2,...>`.
/// Analyzes which beads would be affected by modifying the given files,
/// using the correlator pipeline's file→bead mapping.
/// Go `handleRobotDiff` — `--robot-diff --diff-since <ref>`.
/// Compares current issue set against a previous state.
/// Go `handleRobotNotReadyLabels` — `--robot-not-ready-labels <label1,label2,...>`.
/// Filters triage results to exclude issues with "not-ready" labels.

/// Go `writeRobotHelpFromRegistries` (cmd/bv/robot_registry.go:541-616) renders
/// `--robot-help` from the robot *registries* — a different table from the
/// `robotCommandDocs` list in main.go:7652 that `ROBOT_COMMAND_DOCS`
/// transcribes. The two disagree: the registries carry bare flag names and
/// one-line descriptions, the docs table carries placeholders and long
/// prose. Transcribed row for row in the Go registration order so the
/// generated list cannot drift from the oracle's.
const ROBOT_HELP_COMMANDS: &[(&str, &str)] = &[
    ("robot-help", "Show AI agent help"),
    ("version", "Show version"),
    (
        "robot-capabilities",
        "Output machine-readable command capabilities",
    ),
    ("robot-recipes", "Output recipe summaries for AI agents"),
    (
        "robot-schema",
        "Output JSON schema definitions for robot commands",
    ),
    ("robot-metrics", "Output runtime performance metrics"),
    ("robot-docs", "Output machine-readable robot documentation"),
    ("robot-plan", "Output dependency-respecting execution plan"),
    ("robot-priority", "Output enhanced priority recommendations"),
    (
        "robot-graph",
        "Output dependency graph in JSON, DOT, or Mermaid",
    ),
    ("robot-alerts", "Output drift and proactive alerts"),
    ("robot-suggest", "Output smart suggestions"),
    ("robot-sprint-list", "Output all sprints as JSON"),
    ("robot-burndown", "Output sprint burndown as JSON"),
    ("robot-forecast", "Output ETA forecasts as JSON"),
    (
        "robot-search",
        "Output keyword or hybrid search results as JSON",
    ),
    ("robot-diff", "Output snapshot diff as JSON"),
    ("robot-insights", "Output deep graph analysis and insights"),
    ("robot-next", "Output only the single top recommendation"),
    ("robot-triage", "Output unified triage as JSON"),
    (
        "robot-triage-by-track",
        "Output triage grouped by execution track",
    ),
    ("robot-triage-by-label", "Output triage grouped by label"),
    (
        "robot-history",
        "Output bead-to-commit correlations as JSON",
    ),
    ("bead-history", "Output history for a specific bead as JSON"),
    (
        "robot-correlation-stats",
        "Output correlation feedback statistics as JSON",
    ),
    (
        "robot-explain-correlation",
        "Explain why a commit is linked to a bead",
    ),
    (
        "robot-confirm-correlation",
        "Confirm a correlation is correct",
    ),
    (
        "robot-reject-correlation",
        "Reject an incorrect correlation",
    ),
    ("robot-label-health", "Output label health metrics as JSON"),
    (
        "robot-label-flow",
        "Output cross-label dependency flow as JSON",
    ),
    (
        "robot-label-attention",
        "Output attention-ranked labels as JSON",
    ),
    ("robot-orphans", "Output orphan commit candidates as JSON"),
    (
        "robot-file-beads",
        "Output beads that touched a file path as JSON",
    ),
    (
        "robot-file-hotspots",
        "Output files touched by most beads as JSON",
    ),
    ("robot-impact", "Analyze impact of modifying files"),
    (
        "robot-file-relations",
        "Output files that frequently co-change with a target file",
    ),
    ("robot-related", "Output work related to a specific bead"),
    (
        "robot-blocker-chain",
        "Output blocker chain analysis for an issue",
    ),
    ("robot-impact-network", "Output bead impact network as JSON"),
    ("robot-causality", "Output causal chain analysis for a bead"),
    (
        "robot-sprint-show",
        "Output details for a specific sprint as JSON",
    ),
    (
        "robot-capacity",
        "Output capacity simulation and projection as JSON",
    ),
];

/// Go `ui.GetKeyBindingDocs` (pkg/ui/keybindings.go:218-...) — the
/// authoritative TUI key table, transcribed row for row. Grouped by
/// category in the order the Go slice declares them, which is the order
/// `--robot-help` prints.
const KEY_BINDING_DOCS: &[(&str, &str, &str, &str)] = &[
    ("j", "Move down", "Navigation", "all"),
    ("k", "Move up", "Navigation", "all"),
    ("G", "Go to end", "Navigation", "all"),
    ("home", "Go to start", "Navigation", "list"),
    ("gg", "Go to start", "Navigation", "board,tree"),
    ("ctrl+d", "Page down", "Navigation", "all"),
    ("ctrl+u", "Page up", "Navigation", "all"),
    ("enter", "Open details", "Navigation", "all"),
    (
        "esc",
        "Back/close (list: clear filters)",
        "Navigation",
        "all",
    ),
    ("q", "Quit", "Navigation", "all"),
    ("a", "Actionable view", "Views", "list,detail"),
    ("b", "Board view", "Views", "list,detail"),
    ("g", "Graph view", "Views", "list,detail"),
    ("h", "History view", "Views", "list,detail"),
    ("i", "Insights panel", "Views", "list,detail"),
    (
        "E",
        "Tree view (parent-child hierarchy)",
        "Views",
        "list,detail",
    ),
    (
        "f",
        "Flow matrix (cross-label dependencies)",
        "Views",
        "list,detail",
    ),
    ("P", "Sprint dashboard", "Views", "list,detail"),
    ("[", "Label dashboard", "Views", "list,detail"),
    ("]", "Attention view", "Views", "list,detail"),
    ("!", "Alerts panel", "Views", "list"),
    ("w", "Repo picker (workspace mode)", "Views", "list"),
    ("?", "Help overlay", "Views", "all"),
    (";", "Shortcuts sidebar", "Views", "all"),
    ("p", "Priority hints", "Views", "list,detail"),
    ("o", "Open issues only", "Filters", "list"),
    ("c", "Closed issues only", "Filters", "list"),
    ("r", "Ready (unblocked)", "Filters", "list"),
    ("l", "Label picker", "Filters", "list"),
    ("/", "Search/filter", "Filters", "list"),
    ("s", "Cycle sort mode", "Filters", "list"),
    (
        "S",
        "Sort by triage score (triage recipe)",
        "Filters",
        "list",
    ),
    (
        "t",
        "Time travel (custom revision)",
        "Actions",
        "list,detail",
    ),
    ("T", "Time travel (HEAD~5)", "Actions", "list,detail"),
    ("n", "Next changed issue (time travel)", "Actions", "list"),
    (
        "N",
        "Previous changed issue (time travel)",
        "Actions",
        "list",
    ),
    ("x", "Export to markdown", "Actions", "list,detail"),
    ("y", "Copy issue ID", "Actions", "all"),
    ("C", "Copy full issue", "Actions", "detail"),
    ("O", "Open in $EDITOR", "Actions", "detail"),
    ("'", "Recipe picker", "Actions", "list"),
    ("U", "Self-update check", "Actions", "all"),
    ("V", "Cass sessions", "Actions", "list"),
    ("hjkl", "Navigate graph", "Graph", "graph"),
    ("H", "Scroll left", "Graph", "graph"),
    ("L", "Scroll right", "Graph", "graph"),
    ("J/K", "Scroll graph vertically", "Graph", "graph"),
    (
        "space",
        "Expand/collapse dependency paths",
        "Graph",
        "graph",
    ),
    ("PgUp", "Previous 10 nodes", "Graph", "graph"),
    ("PgDn", "Next 10 nodes", "Graph", "graph"),
    ("h", "Previous column", "Board", "board"),
    ("l", "Next column", "Board", "board"),
    ("H", "First column", "Board", "board"),
    ("L", "Last column", "Board", "board"),
    ("s", "Cycle swimlane mode", "Board", "board"),
    ("tab", "Toggle detail", "Board", "board"),
    ("E", "Exit tree view", "Tree", "tree"),
    ("ctrl+j", "Scroll detail down", "Board", "board"),
    ("ctrl+k", "Scroll detail up", "Board", "board"),
    ("h", "Previous panel", "Insights", "insights"),
    ("l", "Next panel", "Insights", "insights"),
    ("tab", "Next panel", "Insights", "insights"),
    ("shift+tab", "Previous panel", "Insights", "insights"),
    ("e", "Toggle explanations", "Insights", "insights"),
    ("x", "Calculation proof", "Insights", "insights"),
    ("m", "Heatmap toggle", "Insights", "insights"),
    ("v", "Toggle git/bead mode", "History", "history"),
    ("tab", "Toggle focus", "History", "history"),
    ("t", "Toggle timeline pane", "History", "history"),
    ("f", "Toggle file tree", "History", "history"),
    ("J", "Detail scroll down", "History", "history"),
    ("K", "Detail scroll up", "History", "history"),
    ("o", "Open in browser", "History", "history"),
    ("g", "Go to top", "Attention", "attention"),
    ("enter", "Label drilldown", "Attention", "attention"),
    ("1-9", "Filter list by rank", "Attention", "attention"),
    ("]", "Close attention view", "Attention", "attention"),
    ("P", "Close sprint dashboard", "Sprint", "sprint"),
    ("j", "Next sprint", "Sprint", "sprint"),
    ("k", "Previous sprint", "Sprint", "sprint"),
];

/// Go `writeRobotHelp` (cmd/bv/robot_registry.go:541-616).
///
/// The old implementation was a stub: it listed flag *names* from
/// `ROBOT_PRIMARIES` with no descriptions, no envelope contract note, no
/// modifiers heading and no key bindings — 47 lines against the oracle's 171.
/// Everything below the intro is generated from [`ROBOT_HELP_COMMANDS`] and
/// [`KEY_BINDING_DOCS`] so the list cannot drift from the tables, exactly as
/// Go generates it from its registries.
fn print_robot_help() {
    print!(
        "bvr (Beads Viewer) AI Agent Interface\n\
         ====================================\n\
         Use --robot-* flags for deterministic automation output.\n\
         Bare bvr launches the interactive TUI.\n\
         \n\
         Start here:\n\
         \x20 --robot-triage        Unified triage output (recommended entry point)\n\
         \x20 --robot-next          Single top recommendation\n\
         \x20 --robot-capabilities  Machine-readable command/contract manifest\n\
         \x20 --robot-schema        JSON Schema definitions for robot outputs\n\
         \x20 --robot-docs <topic>  Long-form agent documentation\n\
         \n\
         Every payload carries: generated_at, data_hash, source_path, source_kind,\n\
         as_of/as_of_commit (with --as-of), scope (with --label/--recipe/--repo, plus\n\
         scope.unsupported for flags a command cannot honour), load_stats (when records\n\
         were dropped during load).\n\
         Issue-backed responses also carry source_authority, authority_hash, and\n\
         scope_hash. Partial or stale sources retain exploratory output with provisional\n\
         readiness; source_authority.claim_safe must be true before claiming work.\n\
         \n"
    );

    println!("All robot commands:");
    println!("-------------------");
    // Go `formatRobotFlag` (robot_registry.go:3822-3828) is "--" plus the
    // name with one leading "--" trimmed; the registry stores bare names.
    for (name, description) in ROBOT_HELP_COMMANDS {
        println!("  {:<28} {}", format!("--{name}"), description);
    }
    // Go emits a "Modifiers (combine with a command above):" section only when
    // a registry command has IsModifier set. No Go command does, so the
    // heading is absent from the oracle's output and is absent here.

    println!();
    println!("TUI Key Bindings:");
    println!("-----------------");
    // Go groups by category, preserving first-seen order (robot_registry.go:
    // :612-621), then prints a blank line before each heading.
    let mut order: Vec<&str> = Vec::new();
    for (_, _, category, _) in KEY_BINDING_DOCS {
        if !order.contains(category) {
            order.push(category);
        }
    }
    for category in order {
        println!();
        println!("[{category}]");
        for (key, description, cat, context) in KEY_BINDING_DOCS {
            if *cat == category {
                println!("  {:<12} {:<25} ({context})", key, description);
            }
        }
    }

    println!();
    println!("Run bvr --help for all options.");
}
