//! bv-core: frozen data model, tolerant JSONL loader, datasource discovery.
//! Contracts here are api-freeze-v1 — see AGENTS.md and plan §3.1.

pub mod agents;
pub mod model;
pub mod sprint;

pub use model::{Comment, Dependency, DependencyType, Issue, Sprint, Status, ValidationError};

pub mod bdcli;
pub mod data_hash;
pub mod discovery;
pub mod fingerprint;
pub mod loader;
pub mod repo_filter;
pub mod sqlite;
pub mod tracker;

pub mod workspace;

pub use repo_filter::filter_by_repo;
