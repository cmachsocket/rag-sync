//! rag-sync 的可测试核心。
//!
//! 这些模块原先只在 `main.rs` 里用 `mod` 声明，也就是整个 crate 只有 bin target。
//! 集成测试（`tests/` 目录）只有 bin target 时无法 `use` 这些模块 —— 只能退回到
//! `#[cfg(test)] mod tests` 写在源文件内部，或在测试里重抄一份逻辑（那样测的就不是
//! 真正跑的那份代码了）。拆出 lib target 后，`tests/` 下的测试可以直接调用
//! `create_output_files` / `upload_file` 这些真实函数。
//!
//! `main.rs` 只剩 CLI 组装逻辑，逻辑全部走 `rag_sync::` 引用。

pub mod database;
pub mod db_model;
pub mod file_mime;
pub mod http_util;
pub mod lightrag_model;
pub mod lightrag_requests;
pub mod mineru_model;
pub mod mineru_requests;
pub mod schema;
pub mod sync_config;
pub mod sync_folder;
