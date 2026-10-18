use reqwest::StatusCode;
use serde::Deserialize;


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

#[derive(Deserialize,Debug)]
pub struct MetadataResponse {
    pub parse_engine: String,
    pub parse_format: String,
    pub chunk_method: String,
    pub chunk_opts: String,
    pub mm_chunks: u64,
    pub kg_write_state: String,
    pub process_options: String,
}


#[derive(Deserialize,Debug)]
pub struct StatusCodeResponse {
    pub processing: u64,
}

#[derive(Deserialize,Debug)]
pub struct TrackResponse {
    pub id: String,
    pub status: String,
    pub chunks_count: u64,
    pub content_length: u64,
    pub file_path: String,
    pub metadata: MetadataResponse,
    pub total_count: u64,
    pub status_summary: StatusCodeResponse,
}