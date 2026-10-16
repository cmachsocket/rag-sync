use serde::Serialize;
use serde::Deserialize;

#[derive(Serialize)]
pub struct FileUploadRequest {
    pub file_path: String,
    pub file_size: u64,
    pub mime_type: String,
}

#[derive(Deserialize)]
pub struct FileUploadResponse {
    pub id: String,
    pub object: String,
    pub bytes: u64,
    pub created_at: u64,
    pub expires_at: u64,
    pub filename: String,
    pub purpose: String,
    pub mime_type: String,
    pub sha256sum: String,
    pub status: String,
    pub upload_url: String,
    pub upload_method: String,
    pub upload_headers: std::collections::HashMap<String, String>,
    pub file: Option<String>,
}

/*{
  "id": "upload_36838fa63fbb3f71accff2f5",
  "object": "upload",
  "bytes": 758482,
  "created_at": 1792180373,
  "expires_at": 1792183973,
  "filename": "test.pdf",
  "purpose": "parse",
  "mime_type": "application/pdf",
  "sha256sum": "aea7aae52dc22592e5d7fab2bcb3bd87dd64513a13b5a4354b54b0951fa95b3c",
  "status": "pending",
  "upload_url": "http://127.0.0.1:9777/v1/uploads/upload_36838fa63fbb3f71accff2f5/content",
  "upload_method": "PUT",
  "upload_headers": {"Content-Type": "application/pdf"},
  "file": null
} */

#[derive(Deserialize)]
pub struct FilePutInnerStruct {
    pub id: String,
    pub object: String,
    pub bytes: u64,
    pub created_at: u64,
    pub expires_at: u64,
    pub filename: String,
    pub purpose: String,
    pub sha256sum: String,
}

#[derive(Deserialize)]
pub struct FilePutResponse {
    pub id: String,
    pub object: String,
    pub bytes: u64,
    pub status: String,
    pub upload_url: Option<String>,
    pub upload_method: Option<String>,
    pub file: Option<FilePutInnerStruct>,
}

/*{
  "id": "upload_36838fa63fbb3f71accff2f5",
  "object": "upload",
  "bytes": 758482,
  "status": "completed",
  "upload_url": null,
  "upload_method": null,
  "file": {
    "id": "file-0daba4e68fe97e9ab87218f9",
    "object": "file",
    "bytes": 758482,
    "created_at": 1792180375,
    "expires_at": 1792183973,
    "filename": "test.pdf",
    "purpose": "parse",
    "sha256sum": "aea7aae52dc22592e5d7fab2bcb3bd87dd64513a13b5a4354b54b0951fa95b3c"
  }
} */
#[derive(Serialize)]
pub struct FileSource {
    pub source: FileSourceType,
}

#[derive(Serialize)]
pub struct FileSourceType {
    pub r#type: String,
    pub file_id: Option<String>,
    pub url: Option<String>,
    pub name: Option<String>,
    pub data: Option<String>,
    pub path: Option<String>,
}

#[derive(Serialize)]
pub struct FileParseRequest {
    pub files: Vec<crate::json_model::FileSource>,
}

/*
四种 source 各自的最小形态：

```json
{"files":[{"source":{"type":"file_id","file_id":"..."}}]}
{"files":[{"source":{"type":"url","url":"..."}}]}
{"files":[{"source":{"type":"inline","name":"a.pdf","data":"<base64>"}}]}
{"files":[{"source":{"type":"local","path":"/abs/path.pdf"}}]}
```

*/


#[derive(Deserialize)]
pub struct FileParseResponseFileOutput {
    pub file_id: String,
    pub bytes: u64,
}

#[derive(Deserialize)]
pub struct FileParseResponseFile {
    pub name: String,
    pub status: String,
    pub page_range: String,
    pub error: Option<String>,
    pub output_files: Option<std::collections::HashMap<String, FileParseResponseFileOutput>>,
    pub content: Option<std::collections::HashMap<String, String>>,
}

#[derive(Deserialize)]
pub struct Links {
  #[serde(rename = "self")]
  pub self_link: String,
    pub cancel: String,
}

#[derive(Deserialize)]
pub struct Progress {
    pub completed: u64,
    pub failed: u64,
    pub total: u64,
}

#[derive(Deserialize)]
pub struct FileParseResponse {
    pub status: String,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub tier: String,
    pub output_formats: Vec<String>,
    pub access_level: String,
    pub progress: Option<Progress>,
    pub task_id: String,
    pub status_url: String,
    pub result_url: String,
    pub files: Vec<FileParseResponseFile>,
    pub links: Links,
}

/*

```json
{
  "status": "completed",
  "created_at": 1792180440,
  "started_at": "...",
  "finished_at": "...",
  "tier": "standard",
  "output_formats": ["markdown"],
  "access_level": "...",
  "progress": null,
  "task_id": "job_5f2275d16741485741624f6e",
  "status_url": "...",
  "result_url": "...",
  "links": {"self": "...", "cancel": "..."},
  "files": [
    {
      "name": "test.pdf",
      "status": "completed",
      "page_range": "1-3",
      "error": null,
      "output_files": {
        "markdown": {"file_id": "file-5c1489ebba6abcdad762d6f2", "bytes": 18440},
        "middle_json": null,
        "structured_content": null,
        "html": null,
        "latex": null,
        "docx": null,
        "zip": null
      },
      "content": {"markdown": "<内联的 markdown 字符串，长度 18406>"}
    }
  ]
}
```
{
  "status": "running",
  "created_at": "2026-10-16T21:34:50Z",
  "started_at": "2026-10-16T21:34:50Z",
  "finished_at": null,
  "tier": "standard",
  "output_formats": ["markdown"],
  "access_level": "anonymous",
  "progress": {"completed": 0, "failed": 0, "total": 1},
  "task_id": "job_70062f93f45ac536eeb0e878",
  "status_url": "/v1/tasks/job_70062f93f45ac536eeb0e878",
  "result_url": "/v1/tasks/job_70062f93f45ac536eeb0e878/result"
  "files": [{
    "file_id": "file-9a1806385362e774660f373d",
    "name": "slow.pdf",
    "page_range": "1-30",
    "status": "queued",
    "parse": null,
    "output_files": null,
    "error": null
  }],
  "links": {
    "self": "/v1/tasks/job_70062f93f45ac536eeb0e878",
    "cancel": "/v1/tasks/job_70062f93f45ac536eeb0e878"
  },
}
```
`content.markdown` 直接内联返回完整内容，不必再调 `GET /v1/files/{file_id}/content`；`output_files.markdown.file_id` 另给了一个可寻址的 id。

*/

#[derive(Deserialize)]
pub struct TaskStatusResponse {
    pub completed: i64,
    pub failed: i64,
    pub total: i64,
}

//{"completed": 0, "failed": 0, "total": 1}