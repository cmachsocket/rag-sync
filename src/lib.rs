//! rag-sync 的可测试核心。
//!
//! 拆出 lib target 后 `tests/` 下的集成测试才能 `use` 这些模块，直接调用
//! `create_output_files` / `upload_file` 这些真实函数；`main.rs` 只剩 CLI 组装。

pub mod database;
pub mod db_model;
pub mod file_mime;
pub mod http_util;
pub mod joplin_model;
pub mod joplin_requests;
pub mod joplin_sync;
pub mod lightrag_model;
pub mod lightrag_requests;
pub mod mineru_model;
pub mod mineru_requests;
pub mod schema;
pub mod sync_config;
pub mod sync_folder;
