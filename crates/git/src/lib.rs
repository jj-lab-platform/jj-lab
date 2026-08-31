//! jj-lib 真相源封装 + git 双向同步。
//!
//! 分层：`repo`（jj-lib 真相源封装）、`sync`（clone/fetch/push，全 jj 官方）、
//! `ingest`（git → jj 升格，保留）、`anchor`（change-id 锚定）。

pub mod actions;
pub mod anchor;
pub mod build;
pub mod helm;
pub mod http;
pub mod ingest;
pub mod mutation;
pub mod namespaces;
pub mod ops;
pub mod project;
pub mod read;
pub mod repo;
pub mod runtime;
pub mod scheduler;
pub mod service;
pub mod settings;
pub mod sync;
pub mod task;

pub use repo::RepoStore;