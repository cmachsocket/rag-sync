use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// POST /v1/uploads 请求体，契约为 additionalProperties: false，字段名必须严格一致；
// sha256sum 服务端据此幂等去重。
#[derive(Serialize)]
pub struct FileUploadRequest<'a> {
    /// 只取文件名，不含目录。服务端据此决定解析器与产物命名。
    pub filename: &'a str,
    pub bytes: u64,
    pub mime_type: &'a str,
    pub sha256sum: &'a str,
}

// POST /v1/uploads 与 GET /v1/uploads/{id} 共用的响应；
// 按 sha256 幂等命中时 file 为 null，故为 Option。
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

// POST /v1/parse/jobs 的请求体，同为 additionalProperties: false，多塞字段直接 422。
#[derive(Serialize)]
pub struct CreateJobRequest {
    pub files: Vec<JobFileEntry>,
}

// 不建模 page_range：不传就是整份文档，而我们要的从来不是「解析某几页」。
#[derive(Serialize)]
pub struct JobFileEntry {
    pub source: FileSource,
}

/// POST /v1/parse/jobs（恒 202）与 GET /v1/parse/jobs/{job_id} 返回同构，故复用。
/// tier / links 等无关字段不建模，serde 解析时忽略。
#[derive(Deserialize)]
pub struct JobResponse {
    pub job_id: String,
    /// queued / running / completed / partial / failed / canceled（一个 l，与 /v1/tasks 的 cancelled 不同）。
    pub status: String,
    #[serde(default)]
    pub progress: Option<Progress>,
    #[serde(default)]
    pub files: Vec<JobFileResult>,
}

/// 单个文件的结果：没有内联 content，markdown 要拿 output_files.markdown.file_id 去 Files API 取。
#[derive(Deserialize)]
pub struct JobFileResult {
    pub name: String,
    /// queued / running / completed / failed
    pub status: String,
    #[serde(default)]
    pub output_files: Option<HashMap<String, Option<OutputFileRef>>>,
    /// ErrorDetail 留成 Value：错误正文是排查时唯一的线索，解析失败别带崩整份响应。
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

/// 轮询进度，progress 是嵌套对象。
#[derive(Deserialize)]
pub struct Progress {
    pub completed: u64,
    pub failed: u64,
    pub total: u64,
}
