use crate::db_model;
use crate::mineru_model::{
    FileParseItem, FileParseRequest, FileSource, OutputFileRef, TaskResponse, UploadObject,
};
use crate::sync_config::SyncConfig;
use reqwest::Body;
use std::path::Path;
use std::time::Duration;
use tokio_util::io::ReaderStream;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// 轮询上限。仅防服务端任务卡死导致无限等待，与 HTTP 层的 wait_timeout 无关。
const MAX_POLL_ROUNDS: u32 = 720;

/// 错误信息统一带上文件路径，便于定位是哪个文件出的问题。
fn err(file: &db_model::File, msg: impl std::fmt::Display) -> Box<dyn std::error::Error> {
    format!("{}: {msg}", file.path).into()
}

/// 状态不是 2xx 时返回带正文与文件路径的错误，否则原样交给调用方。
async fn ensure_success(
    resp: reqwest::Response,
    file: &db_model::File,
    动作: &str,
) -> Result<reqwest::Response> {
    let status = resp.status();
    if !status.is_success() {
        // body 必须先读完：错误原因（字段校验、限流…）几乎都只在 body 里
        let body = crate::http_util::error_body(resp).await;
        return Err(err(file, format!("{动作}被拒绝（{status}），响应: {body}")));
    }
    Ok(resp)
}


async fn upload(
    client: &reqwest::Client,
    file: &db_model::File,
    config: &SyncConfig,
) -> Result<String> {
    let path = Path::new(&file.path);
    // filename 只取文件名部分，含目录会被服务端拒绝或产生意外的产物名
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| err(file, "文件名不是合法 UTF-8"))?;

    let bytes = tokio::fs::metadata(path)
        .await
        .map_err(|e| err(file, format!("读取文件元数据失败: {e}")))?
        .len();

    if file.sha256.is_empty() {
        return Err(err(file, "缺少 sha256，无法声明上传"));
    }

    let mime_type = crate::file_mime::mime_for(path);
    let req = crate::mineru_model::FileUploadRequest {
        filename,
        bytes,
        mime_type,
        sha256sum: &file.sha256,
    };

    let resp = client
        .post(format!("{}/v1/uploads", config.mineru_url))
        .json(&req)
        .send()
        .await
        .map_err(|e| err(file, format!("创建上传失败: {e}")))?;
    let resp = ensure_success(resp, file, "创建上传").await?;
    let upload: UploadObject = resp
        .json()
        .await
        .map_err(|e| err(file, format!("解析上传响应失败: {e}")))?;

    // 按 sha256 幂等：内容已存在时直接返回 completed，upload_url 为 null，
    // 再 PUT 会得到 409 upload_already_terminal。
    if upload.status == "completed" {
        if let Some(f) = upload.file {
            return Ok(f.id);
        }
        return Err(err(file, "上传已完成但响应中缺少 file.id"));
    }

    put_content(client, file, config, &upload.id, mime_type).await?;
    complete(client, file, config, &upload.id).await
}

/// 流式上传文件内容，避免把整个文件读进内存。
async fn put_content(
    client: &reqwest::Client,
    file: &db_model::File,
    config: &SyncConfig,
    upload_id: &str,
    mime_type: &str,
) -> Result<()> {
    let f = tokio::fs::File::open(&file.path)
        .await
        .map_err(|e| err(file, format!("打开文件失败: {e}")))?;

    // ReaderStream 要求 AsyncRead + Unpin，tokio::fs::File 两者都满足
    let stream = ReaderStream::with_capacity(f, 64 * 1024);
    let body_len = tokio::fs::metadata(&file.path)
        .await
        .map_err(|e| err(file, format!("读取文件元数据失败: {e}")))?
        .len();

    let resp = client
        .put(format!("{}/v1/uploads/{upload_id}/content", config.mineru_url))
        .header("Content-Type", mime_type)
        // 声明长度，避免服务端使用 chunked 编码
        .header("Content-Length", body_len)
        .body(Body::wrap_stream(stream))
        .send()
        .await
        .map_err(|e| err(file, format!("上传内容失败: {e}")))?;

    ensure_success(resp, file, "上传内容").await?;
    Ok(())
}

/// 触发完成，一次 POST 即完成，不需要轮询。
async fn complete(
    client: &reqwest::Client,
    file: &db_model::File,
    config: &SyncConfig,
    upload_id: &str,
) -> Result<String> {
    let resp = client
        .post(format!("{}/v1/uploads/{upload_id}/complete", config.mineru_url))
        .send()
        .await
        .map_err(|e| err(file, format!("完成上传请求失败: {e}")))?;
    let resp = ensure_success(resp, file, "完成上传").await?;
    let upload: UploadObject = resp
        .json()
        .await
        .map_err(|e| err(file, format!("解析完成响应失败: {e}")))?;

    if upload.status != "completed" {
        return Err(err(file, format!("上传未完成，状态: {}", upload.status)));
    }
    upload
        .file
        .map(|f| f.id)
        .ok_or_else(|| err(file, "完成响应中缺少 file.id"))
}

/// 触发解析。202 时轮询到终态并取回产物内容。
async fn parse(
    client: &reqwest::Client,
    file: &db_model::File,
    config: &SyncConfig,
    file_id: &str,
) -> Result<String> {
    let req = FileParseRequest {
        files: vec![FileParseItem {
            source: FileSource::from_file_id(file_id.to_string()),
        }],
    };

    let resp = client
        .post(format!("{}/v1/file_parse", config.mineru_url))
        .json(&req)
        .send()
        .await
        .map_err(|e| err(file, format!("解析请求失败: {e}")))?;

    let resp = ensure_success(resp, file, "解析").await?;

    // 202 与 200 同构，都反序列化成 TaskResponse
    let mut task: TaskResponse = resp
        .json()
        .await
        .map_err(|e| err(file, format!("解析响应失败: {e}")))?;

    if task.status == "running" {
        task = poll(client, file, config, &task.task_id).await?;
    }

    // 终态校验。注意 status 取值为 completed/failed/cancelled，没有 "success"
    if task.status != "completed" {
        let detail = task
            .files
            .first()
            .and_then(|f| f.error.as_ref())
            .map(|e| format!("，错误: {e}"))
            .unwrap_or_default();
        return Err(err(file, format!("解析未成功，状态: {}{detail}", task.status)));
    }

    extract_markdown(client, file, config, &task).await
}

async fn poll(
    client: &reqwest::Client,
    file: &db_model::File,
    config: &SyncConfig,
    task_id: &str,
) -> Result<TaskResponse> {
    for round in 1..=MAX_POLL_ROUNDS {
        let resp = client
            .get(format!("{}/v1/tasks/{task_id}", config.mineru_url))
            .send()
            .await
            .map_err(|e| err(file, format!("轮询失败: {e}")))?;
        let resp = ensure_success(resp, file, "轮询").await?;
        let task: TaskResponse = resp
            .json()
            .await
            .map_err(|e| err(file, format!("解析轮询响应失败: {e}")))?;

        // 终态有四种，不能只判 completed，否则失败任务会一直轮询
        if matches!(
            task.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            return Ok(task);
        }

        if let Some(p) = &task.progress {
            println!(
                "[轮询 {round}/{MAX_POLL_ROUNDS}] {} 状态: {} ({}/{})",
                file.path, task.status, p.completed, p.total
            );
        } else {
            println!("[轮询 {round}/{MAX_POLL_ROUNDS}] {} 状态: {}", file.path, task.status);
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Err(err(file, "轮询超时，任务可能仍在服务端运行"))
}

/// 取 markdown 内容。同步完成时内联在 content 里，轮询完成后需二次请求。
async fn extract_markdown(
    client: &reqwest::Client,
    file: &db_model::File,
    config: &SyncConfig,
    task: &TaskResponse,
) -> Result<String> {
    let parsed = task
        .files
        .first()
        .ok_or_else(|| err(file, "响应中缺少 files 条目"))?;

    // 优先用内联内容
    if let Some(md) = parsed.content.as_ref().and_then(|c| c.get("markdown")) {
        if let Some(text) = md {
            return Ok(text.clone());
        }
    }

    // 回退到产物 id：轮询终态的 content 恒为空，必须走这条路
    let product_id = parsed
        .output_files
        .as_ref()
        .and_then(|m| m.get("markdown"))
        .and_then(|o| o.as_ref())
        .map(|o: &OutputFileRef| o.file_id.as_str())
        .ok_or_else(|| err(file, "既无内联内容也无 markdown 产物 id"))?;

    let resp = client
        .get(format!("{}/v1/files/{product_id}/content", config.mineru_url))
        .send()
        .await
        .map_err(|e| err(file, format!("获取解析内容失败: {e}")))?;
    let resp = ensure_success(resp, file, "获取解析内容").await?;
    resp.text()
        .await
        .map_err(|e| err(file, format!("读取解析内容失败: {e}")))
}

/// 单个文件的完整流程：上传 → 解析 → 返回 markdown。
///
/// 每轮都重新上传：`files.id` 是 DB 行号，不是 MinerU 的 file_id。重复上传不花钱
/// 靠的是服务端按 sha256 幂等（见上面 completed 分支）。
pub async fn mineru_tackle(
    client: &reqwest::Client,
    file: &db_model::File,
    config: &SyncConfig,
) -> Result<String> {
    let file_id = upload(client, file, config).await?;
    parse(client, file, config, &file_id).await
}