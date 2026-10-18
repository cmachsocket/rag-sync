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
只有 access_token 是必需的。其余字段带 serde(default)，
服务端增删字段不会让登录整体失效。
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
```
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

/// 文档元数据用 map 而不是定长 struct：字段随流水线阶段增减。
/// 处理中的快照带 `chunk_method` / `chunk_opts` / `process_options`，
/// 终态只剩 `parse_engine` / `parse_format` / `mm_chunks` / `kg_write_state`，
/// 写死字段在任一阶段都会反序列化失败。
pub type MetadataResponse = std::collections::HashMap<String, serde_json::Value>;

/// `status_summary` 是 map，键是状态名、值是计数。
/// 大小写不稳定：OpenAPI 示例给 `"PROCESSED"`，实测响应给 `"processed"`，
/// 只能用 map + 忽略大小写比对，不能按固定字段名反序列化。
pub type StatusSummary = std::collections::HashMap<String, u64>;

#[derive(Deserialize, Debug)]
pub struct DocStatusResponse {
    pub id: String,
    /// DocStatus 枚举：pending / parsing / analyzing / processing /
    /// preprocessed / processed / failed。值是小写。
    pub status: String,
    #[serde(default)]
    pub chunks_count: Option<u64>,
    #[serde(default)]
    pub error_msg: Option<String>,
    #[serde(default)]
    pub content_length: Option<u64>,
    #[serde(default)]
    pub metadata: Option<MetadataResponse>,
}

/// 信封结构：文档在 `documents[]` 里，顶层只有 track_id / total_count /
/// status_summary。**顶层没有 id、没有 status** —— 按顶层解析会静默拿到空值。
#[derive(Deserialize, Debug)]
pub struct TrackResponse {
    pub track_id: String,
    pub documents: Vec<DocStatusResponse>,
    pub total_count: u64,
    pub status_summary: StatusSummary,
}

impl TrackResponse {
    /// 全部文档都进入终态才算处理完。
    /// 上传是异步的：549 B 纯文本要 64 s，8.6 KB Markdown 因为含 4 个表格
    /// 要 728 s，绝不能拿到 200 就当处理完了。
    pub fn is_finished(&self) -> bool {
        self.documents.iter().all(|d| {
            return matches!(
                d.status.to_ascii_lowercase().as_str(),
                "processed" | "failed"
            )
        })
    }

    pub fn first_doc_id(&self) -> Option<&str> {
        self.documents.first().map(|d| d.id.as_str())
    }

    /// 失败时的原因。内容重复（`Content already exists. Original doc_id: xxx`）
    /// 只在异步阶段暴露，且同样以 failed 收场。
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
`status` 枚举只有三个值，全部是 **HTTP 200**，靠状态码分不出来：

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

这就是「如何知道已经成功删除」的答案：光看 200 不够，
必须先判 status != "busy"，再等 pipeline 的 destructive_busy 清掉。
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
`POST /documents/paginated` —— 判定删除是否生效的**唯一权威依据**。

不用 pipeline_status 的原因：`destructive_busy` / `busy` / `scanning` 都是
**全局共享**标志。并发删除、`DELETE /documents`、`/documents/scan` 都会置位，
分不清标志背后是谁的任务 —— 既可能等错对象，也可能在自己的后台任务尚未启动的
瞬间读到 false 而提前判定成功。文档列表没有这种歧义。
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
        DocumentsRequest { page: 1, page_size: 200 }
    }
}