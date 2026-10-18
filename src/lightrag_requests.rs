use tokio::stream;
use tokio_util::codec::FramedRead;

use crate::lightrag_model;
use std::time::Duration;

/// 登录是启动路径上的单次交互，卡住应快速失败。
/// reqwest 的 Client 默认无超时，不加这层保护，主机不可达时 new() 会无限期挂起。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);

/// 轮询间隔。后台流水线是批处理的，太密只是白打服务端。
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// track_status 轮询上限。后台耗时和内容强相关、**不是线性于文件大小**：
/// 549 B 纯文本 64 s，8.6 KB Markdown 因含 4 个表格要 728 s，所以留够 15 分钟。
const TRACK_TIMEOUT: Duration = Duration::from_secs(900);

/// 删除完成等待上限。删除走后台任务，通常远快于抽取，但同样可能撞上
/// 长任务尾巴（pipeline 忙时 delete 会直接返回 busy）。
const DELETE_TIMEOUT: Duration = Duration::from_secs(120);

// 上传接口尚未实现，main 也还没接入，因此暂时没有构造点。
#[allow(dead_code)]
struct RAGRequest<'a> {
    client: &'a reqwest::Client,
    url: &'a str,
    jwt_token: String,
}

impl<'a> RAGRequest<'a> {
    /// 登录失败直接返回 Err，不降级成空 token。
    /// 空 token 会让后续请求以 401 的形式失败，把登录错误伪装成接口错误。
    pub async fn new(
        client: &'a reqwest::Client,
        url: &'a str,
        username: &str,
        password: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let jwt_token = Self::login(client, url, username, password).await?;
        Ok(RAGRequest {
            client,
            url,
            jwt_token,
        })
    }

    async fn login(
        client: &reqwest::Client,
        url: &str,
        username: &str,
        password: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let login_url = format!("{url}/login");
        // 用 .form() 而非手工拼串：它会做 urlencoded 编码，
        // 密码含 & = + % 或非 ASCII 时不会被截断成错误参数
        let response = tokio::time::timeout(
            LOGIN_TIMEOUT,
            client
                .post(&login_url)
                .form(&[
                    ("grant_type", "password"),
                    ("username", username),
                    ("password", password),
                ])
                .send(),
        )
        .await
        .map_err(|_| format!("登录请求超时（{LOGIN_TIMEOUT:?}）: {login_url}"))?
        .map_err(|e| format!("登录请求失败: {e}"))?;

        // 先读完 body 再判状态：401 的真实原因（Incorrect username or password）
        // 只在 body 里，只报状态码等于没给线索
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("登录失败，状态码: {status}，响应: {body}").into());
        }

        let login_response: lightrag_model::LoginResponse = serde_json::from_str(&body)
            .map_err(|e| format!("解析登录响应失败: {e}，响应: {body}"))?;
        Ok(login_response.access_token)
    }
    async fn _upload(&self, file_path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let upload_url = format!("{}/documents/upload", self.url);
        let file = tokio::fs::File::open(file_path)
            .await
            .map_err(|e| format!("打开文件失败: {e}"))?;
        let stream = FramedRead::new(file, tokio_util::codec::BytesCodec::new());
        let body = reqwest::Body::wrap_stream(stream);
        // file_name 必须给 basename，不能传完整路径：服务端拿 filename 判同名，
        // `/home/a/b.md` 会让它按整条路径当键，判重和落盘目录都会跑偏。
        // mime_str 不用设，服务端只看扩展名（见 lightrag-upload-endpoint-test.md §3.1）。
        let name = std::path::Path::new(file_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| file_path.to_string());
        let part = reqwest::multipart::Part::stream(body).file_name(name);
        let form = reqwest::multipart::Form::new().part("file", part);
        let resp = self
            .client
            .post(&upload_url)
            .bearer_auth(&self.jwt_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("上传文件请求失败: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            let resp_json: lightrag_model::UploadResponse = resp
                .json()
                .await
                .map_err(|e| format!("解析上传响应失败: {e}"))?;
            // 枚举是 success / partial_success / failure，只有 success 有 track_id。
            if resp_json.status == "success" {
                Ok(resp_json.track_id)
            } else {
                Err(format!(
                    "上传文件失败，状态码: {status}，响应: {:?}",
                    resp_json.message
                )
                .into())
            }
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(format!("上传文件失败，状态码: {status}，响应: {body}").into())
        }
    }

    /// 轮询 track_status 直到所有文档进入终态，返回最后一次响应。
    /// 拿 track_id 的瞬间就调一次是没有意义的 —— 上传响应 31 ms 返回，
    /// 后台抽取要 64 s ~ 728 s。
    async fn _track(
        &self,
        track_id: &str,
    ) -> Result<lightrag_model::TrackResponse, Box<dyn std::error::Error>> {
        // 路径是 track_status，不是 track。
        let track_url = format!("{}/documents/track_status/{track_id}", self.url);
        let deadline = tokio::time::Instant::now() + TRACK_TIMEOUT;
        loop {
            let resp = self
                .client
                .get(&track_url)
                .bearer_auth(&self.jwt_token)
                .send()
                .await
                .map_err(|e| format!("获取文件状态请求失败: {e}"))?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("获取文件状态失败，状态码: {status}，响应: {body}").into());
            }
            let track: lightrag_model::TrackResponse = resp
                .json()
                .await
                .map_err(|e| format!("解析获取文件状态响应失败: {e}"))?;

            if track.is_finished() {
                // failed 也算终态，交给调用方从 error_msg 读原因，
                // 在这里吞掉会让「处理失败」看起来像「还在跑」。
                return Ok(track);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "等待 {track_id} 处理超时（{TRACK_TIMEOUT:?}），当前: {:?}",
                    track.status_summary
                )
                .into());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    pub async fn delete_document(&self, file_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        let delete_url = format!("{}/documents/delete_document", self.url);
        let delete_request = lightrag_model::DeleteDocRequest {
            doc_ids: vec![file_id.to_string()],
            delete_file: true,
            delete_llm_cache: false,
        };
        let resp = self
            .client
            .delete(&delete_url)
            .bearer_auth(&self.jwt_token)
            .json(&delete_request)
            .send()
            .await
            .map_err(|e| format!("删除文档请求失败: {e}"))?;
        let status = resp.status();
        // 先把 body 读完：busy 的说明只在 body 里。
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("删除文档失败，状态码: {status}，响应: {body}").into());
        }

        // 三个状态全是 HTTP 200，光看状态码分不出来。
        // busy / not_allowed 意味着**什么都没删**，当成成功就是静默数据不一致。
        let result: lightrag_model::DeleteDocByIdResponse = serde_json::from_str(&body)
            .map_err(|e| format!("解析删除响应失败: {e}，响应: {body}"))?;
        match result.status.as_str() {
            "deletion_started" => {}
            "busy" => {
                return Err(format!(
                    "删除被拒（pipeline 被其他写者占用，未删除任何内容）: {}",
                    result.message
                )
                .into());
            }
            other => {
                return Err(format!("删除未调度，status={other}: {}", result.message).into());
            }
        }

        // deletion_started 只代表已调度，删除在后台跑。
        // 判定完成只看一件事：文档列表里 doc_id 消失。
        // 不用 pipeline_status —— destructive_busy 是全局共享标志，并发删除 /
        // documents.clear / scan 都会把它置上，分不清是谁的任务在跑；更糟的是
        // 在自己的后台任务还没启动的瞬间读到 false，会提前判定成功。
        self.wait_document_gone(file_id).await?;
        Ok(())
    }

    /// 轮询文档列表直到 doc_id 消失。
    async fn wait_document_gone(&self, doc_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        let deadline = tokio::time::Instant::now() + DELETE_TIMEOUT;
        loop {
            if !self.document_exists(doc_id).await? {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "等待 {doc_id} 从文档列表消失超时（{DELETE_TIMEOUT:?}）"
                )
                .into());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// 文档是否还在库里。查 paginated 列表，是判定删除是否生效的唯一权威依据。
    /// 必须翻完所有页：服务端 page_size 上限 200，只看第一页会漏判。
    async fn document_exists(&self, doc_id: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let url = format!("{}/documents/paginated", self.url);
        let mut req = lightrag_model::DocumentsRequest::first_page();
        loop {
            let resp = self
                .client
                .post(&url)
                .bearer_auth(&self.jwt_token)
                .json(&req)
                .send()
                .await
                .map_err(|e| format!("查询文档列表失败: {e}"))?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("查询文档列表失败，状态码: {status}，响应: {body}").into());
            }
            let page: lightrag_model::PaginatedDocsResponse = resp
                .json()
                .await
                .map_err(|e| format!("解析文档列表失败: {e}"))?;
            if page.documents.iter().any(|d| d.id == doc_id) {
                return Ok(true);
            }
            if !page.pagination.has_next {
                return Ok(false);
            }
            req.page += 1;
        }
    }

    /// 上传并等待后台处理完成，返回 doc_id。
    pub async fn upload_file(&self, file_path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let track_id = self._upload(file_path).await?;
        let track = self._track(&track_id).await?;
        if let Some(err) = track.first_error() {
            return Err(format!("{file_path} 后台处理失败: {err}").into());
        }
        track
            .first_doc_id()
            .map(str::to_string)
            .ok_or_else(|| format!("{track_id} 没有返回任何 doc_id").into())
    }
}
