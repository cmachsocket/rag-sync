use serde::{Deserialize, Serialize};

/// 登录响应：只有 `access_token` 必需，其余带 `serde(default)`。
#[derive(Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default)]
    pub auth_configured: bool,
    #[serde(default)]
    pub auth_mode: String,
    #[serde(default)]
    pub core_version: String,
    #[serde(default)]
    pub api_version: String,
}

/// 上传响应：`status` / `message` / `track_id`。
#[derive(Deserialize)]
pub struct UploadResponse {
    pub status: String,
    pub message: String,
    pub track_id: String,
}

/// 文档元数据用 map 而非定长 struct：字段随流水线阶段增减，写死字段会反序列化失败。
pub type MetadataResponse = std::collections::HashMap<String, serde_json::Value>;

/// `status_summary` 键是状态名、值是计数，大小写不稳定，只能用 map + 忽略大小写比对。
pub type StatusSummary = std::collections::HashMap<String, u64>;

/// 单个文档的状态。`file_path` 是上传时的 filename，判同名冲突靠它反查 doc_id。
#[derive(Deserialize, Debug)]
pub struct DocStatusResponse {
    pub id: String,
    /// DocStatus 枚举：pending / parsing / analyzing / processing /
    /// preprocessed / processed / failed。值是小写。
    pub status: String,
    /// **有符号**：失败的文档服务端给 `-1`，写成 `u64` 会让整个响应反序列化失败。
    #[serde(default)]
    pub chunks_count: Option<i64>,
    #[serde(default)]
    pub error_msg: Option<String>,
    #[serde(default)]
    pub content_length: Option<u64>,
    pub file_path: String,
    #[serde(default)]
    pub metadata: Option<MetadataResponse>,
}

/// 信封结构：文档在 `documents[]` 里，顶层**没有 id、没有 status**，按顶层解析会静默拿到空值。
#[derive(Deserialize, Debug)]
pub struct TrackResponse {
    pub track_id: String,
    pub documents: Vec<DocStatusResponse>,
    pub total_count: u64,
    pub status_summary: StatusSummary,
}

impl TrackResponse {
    /// 全部文档都进入终态才算处理完（上传是异步的，不能拿到 200 就当完成）。
    pub fn is_finished(&self) -> bool {
        // 空数组不算完成：第一次查询可能拿到 documents: []，`all()` 恒为 true。
        !self.documents.is_empty()
            && self.documents.iter().all(|d| {
                return matches!(
                    d.status.to_ascii_lowercase().as_str(),
                    "processed" | "failed"
                );
            })
    }

    pub fn first_doc_id(&self) -> Option<&str> {
        self.documents.first().map(|d| d.id.as_str())
    }

    /// 第一条 failed 文档的原因；内容重复只在异步阶段暴露，同样以 failed 收场。
    pub fn first_error(&self) -> Option<&str> {
        self.documents
            .iter()
            .find(|d| d.status.eq_ignore_ascii_case("failed"))
            .and_then(|d| d.error_msg.as_deref())
    }
}

/// DELETE `/documents/delete_document` 的请求体：DELETE 带 JSON body，只有 `doc_ids` 必填。
#[derive(Serialize)]
pub struct DeleteDocRequest {
    pub doc_ids: Vec<String>,
    pub delete_file: bool,
    pub delete_llm_cache: bool,
}

/// 删除结果：三个 `status` 全是 HTTP 200，靠状态码分不出来 ——
/// `deletion_started` 已调度但没删完，`busy` 什么都没删，`not_allowed` 拒绝。
#[derive(Deserialize, Debug)]
pub struct DeleteDocByIdResponse {
    pub status: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub doc_id: String,
}

impl DeleteDocByIdResponse {
    pub fn is_scheduled(&self) -> bool {
        self.status == "deletion_started"
    }

    /// 200 + busy 是「被拒绝且无副作用」，不能当成功。
    pub fn is_busy(&self) -> bool {
        self.status == "busy"
    }
}

/// `POST /documents/paginated`：判定删除是否生效的依据。
/// 不用 pipeline_status —— 它的标志位全局共享，会等错对象或提前判定成功。
#[derive(Deserialize, Debug)]
pub struct PaginationInfo {
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub total_pages: u32,
    #[serde(default)]
    pub total_count: u64,
    /// 判断还有没有下一页的主要依据。
    #[serde(default)]
    pub has_next: bool,
}

#[derive(Deserialize, Debug)]
pub struct PaginatedDocsResponse {
    pub documents: Vec<DocStatusResponse>,
    pub pagination: PaginationInfo,
}

#[derive(Serialize)]
pub struct DocumentsRequest {
    pub page: u32,
    /// 服务端限制 10-200，低于 10 或高于 200 会 422。
    pub page_size: u32,
}

impl DocumentsRequest {
    pub fn first_page() -> Self {
        DocumentsRequest {
            page: 1,
            page_size: 200,
        }
    }
}
