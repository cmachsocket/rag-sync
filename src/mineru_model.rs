use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// POST /v1/uploads 请求体。契约为 additionalProperties: false，字段名必须严格一致。
// sha256sum 可选，但传了服务端才能幂等去重，本地算好的 sha256 直接复用。
#[derive(Serialize)]
pub struct FileUploadRequest<'a> {
    /// 只取文件名，不含目录。服务端据此决定解析器与产物命名。
    pub filename: &'a str,
    pub bytes: u64,
    pub mime_type: &'a str,
    pub sha256sum: &'a str,
}

// POST /v1/uploads 与 GET /v1/uploads/{id} 共用的响应。
// 按 sha256 幂等命中时 upload_url / upload_method / file 均为 null，故全部为 Option。
#[derive(Deserialize)]
pub struct UploadObject {
    pub id: String,
    pub status: String,
    pub file: Option<FileObject>,
}

#[derive(Deserialize)]
pub struct FileObject {
    pub id: String,
}

// GET /v1/files/{产物 file_id}/content 直接返回 markdown 原文，不是 JSON。
#[derive(Deserialize)]
pub struct OutputFileRef {
    pub file_id: String,
    pub bytes: u64,
}

/// source 的四种变体，只填 type 对应的那一个字段，其余 None 会被跳过。
#[derive(Serialize)]
pub struct FileSource {
    pub r#type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl FileSource {
    pub fn from_file_id(file_id: String) -> Self {
        Self {
            r#type: "file_id",
            file_id: Some(file_id),
            url: None,
            name: None,
            data: None,
            path: None,
        }
    }
}

#[derive(Serialize)]
pub struct FileParseRequest {
    pub files: Vec<FileParseItem>,
}

#[derive(Serialize)]
pub struct FileParseItem {
    pub source: FileSource,
}

/// /v1/file_parse 与 GET /v1/tasks/{id} 返回同构的响应，故复用同一结构体。
/// progress 是嵌套对象。
#[derive(Deserialize)]
pub struct Progress {
    pub completed: u64,
    pub failed: u64,
    pub total: u64,
}

#[derive(Deserialize)]
pub struct ParsedFile {
    pub name: String,
    pub status: String,
    pub error: Option<serde_json::Value>,
    #[serde(default)]
    pub output_files: Option<HashMap<String, Option<OutputFileRef>>>,
    #[serde(default)]
    pub content: Option<HashMap<String, Option<String>>>,
}

#[derive(Deserialize)]
pub struct TaskResponse {
    /// completed / running / failed / cancelled，注意没有 "success"
    pub status: String,
    pub task_id: String,
    pub finished_at: Option<String>,
    #[serde(default)]
    pub progress: Option<Progress>,
    #[serde(default)]
    pub files: Vec<ParsedFile>,
}
