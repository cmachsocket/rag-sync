use serde::{Deserialize, Serialize};

/*
```json
{
  "access_token": "<JWT, 长度 199>",
  "token_type": "bearer",
  "auth_configured": true,
  "auth_mode": "enabled",
  "core_version": "1.5.7",
  "api_version": "0344"
}
```
只有 access_token 必需，其余带 serde(default)。
*/
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

/*
```json
{
  "status": "success",
  "message": "File 'configuration.md' uploaded successfully. Processing will continue in background.",
  "track_id": "upload_20261017_002603_f93bcf1d"
}
*/

#[derive(Deserialize)]
pub struct UploadResponse {
    pub status: String,
    pub message: String,
    pub track_id: String,
}

/*
```json
{
  "id": "doc-a66b35d20295cb764719ac8bd35837ec",
  "status": "processing",
  "chunks_count": 6,
  "content_length": 8869,
  "file_path": "configuration.md",
  "metadata": {
    "parse_engine": "native",
    "parse_format": "lightrag",
    "chunk_method": "paragraph_semantic",
    "chunk_opts": "size=2000, overlap=100",
    "mm_chunks": 4,
    "kg_write_state": "pre_graph",
    "process_options": "teP"
  },
  "total_count": 1,
  "status_summary": {"processing": 1}
}
```
*/
/// 文档元数据用 map 而非定长 struct：字段随流水线阶段增减，写死字段会反序列化失败。
pub type MetadataResponse = std::collections::HashMap<String, serde_json::Value>;

/// `status_summary` 键是状态名、值是计数，大小写不稳定（OpenAPI 给 "PROCESSED"，
/// 实测给 "processed"），只能用 map + 忽略大小写比对。
pub type StatusSummary = std::collections::HashMap<String, u64>;

#[derive(Deserialize, Debug)]
pub struct DocStatusResponse {
    pub id: String,
    /// DocStatus 枚举：pending / parsing / analyzing / processing /
    /// preprocessed / processed / failed。值是小写。
    pub status: String,
    /// **有符号**：失败的文档服务端给 `-1`（track_status 与 documents/paginated 实测）。
    /// 写成 `u64` 会让整个响应反序列化失败 —— 列表里只要有一条 failed 文档，
    /// 所有查询与删除全挂。
    #[serde(default)]
    pub chunks_count: Option<i64>,
    #[serde(default)]
    pub error_msg: Option<String>,
    #[serde(default)]
    pub content_length: Option<u64>,
    /// 服务端存的是上传时的 filename（我们传 basename），判同名冲突靠它反查 doc_id。
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
    /// 全部文档都进入终态才算处理完（上传是异步的，绝不能拿到 200 就当完成）。
    pub fn is_finished(&self) -> bool {
        // 空数组不算完成：拿到 track_id 的那一刻记录未必已注册，第一次查询会拿到
        // documents: []。空迭代器上 all() 恒为 true，这个 !is_empty() 不能省。
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

    /// 失败时的原因。内容重复（`Content already exists. Original doc_id: xxx`）
    /// 只在异步阶段暴露，同样以 failed 收场。
    pub fn first_error(&self) -> Option<&str> {
        self.documents
            .iter()
            .find(|d| d.status.eq_ignore_ascii_case("failed"))
            .and_then(|d| d.error_msg.as_deref())
    }
}

/*
路径是 `DELETE /documents/delete_document`，**DELETE 带 JSON body**（`DeleteDocRequest`），只有 `doc_ids` 必填。
{
        "doc_ids": ["doc-ca1682790e657199eab1b22d3e556f98"],
        "delete_file": true,
        "delete_llm_cache": false
      }'
*/

#[derive(Serialize)]
pub struct DeleteDocRequest {
    pub doc_ids: Vec<String>,
    pub delete_file: bool,
    pub delete_llm_cache: bool,
}

/*
`status` 枚举三个值全是 **HTTP 200**，靠状态码分不出来：

```json
{
  "status": "deletion_started",
  "message": "...",
  "doc_id": "doc-ca1682790e657199eab1b22d3e556f98"
}
```

| status | 含义 |
| --- | --- |
| `deletion_started` | 已在后台调度，**还没删完** |
| `busy` | **什么都没删**，pipeline 被别的写者占着，要重试 |
| `not_allowed` | 拒绝 |
*/
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

/*
`POST /documents/paginated` —— 判定删除是否生效的唯一权威依据。

不用 pipeline_status：它的标志位都是全局共享的（并发删除 / clear / scan 都会置位），
既可能等错对象，也可能在自己的任务尚未启动时读到 false 而提前判定成功。
*/
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
