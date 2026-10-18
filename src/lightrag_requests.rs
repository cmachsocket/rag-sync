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

/// 文档列表翻页上限。只防服务端一直回 `has_next: true` —— 没有它，
/// 一个坏掉的代理就能让同步永远转下去。
const MAX_PAGES: u32 = 1000;

/// 上传失败的原因。**只有同名冲突能自愈**，其余一律原样上抛 ——
/// 在 upload_file 里对 400 / 401 也重试，会把真实错误掩盖成一次莫名其妙的失败。
enum UploadError {
    /// 409。服务端把文件名当唯一文档键，写文件之前就同步拒绝，
    /// detail 里要求先删掉已有记录。detail 原样保留，用于区分冲突来源。
    Conflict(String),
    Fatal(String),
}

impl UploadError {
    fn conflict(detail: &str) -> Self {
        UploadError::Conflict(detail.to_string())
    }

    fn fatal(msg: String) -> Self {
        UploadError::Fatal(msg)
    }

    fn into_err(self, ctx: &str) -> Box<dyn std::error::Error> {
        match self {
            UploadError::Conflict(detail) => {
                format!("{ctx}：仍为 409 同名冲突，服务端信息: {detail}").into()
            }
            UploadError::Fatal(msg) => format!("{ctx}: {msg}").into(),
        }
    }
}

/// 取 basename。上传给服务端的 filename 就是它，服务端存的 file_path 也是它
/// （不是原始绝对路径），两边靠这个对上。
pub fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// LightRAG 判同名用的是 canonical basename：会剥掉 parser hint，
/// `abc.docx` 和 `abc.[native].docx` 指向同一条记录（见 upload 的 OpenAPI 描述）。
///
/// 这里只剥实测见过的两种（§3.2 的 legacy / §3.3 的 native）。没见过的 hint 不猜 ——
/// 宁可匹配不上、报出来让人看一眼，也不要按错误的名字删掉别人的文档。
pub fn canonical_basename(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let mut stem = match lower.rfind('.') {
        Some(dot) => &lower[..dot],
        None => &lower[..],
    };
    // 循环剥而不是剥一次：`a.[native].[legacy].md` 这种叠着的也要归到同一个键。
    loop {
        let mut stripped = false;
        for hint in ["[native]", "[legacy]"] {
            if let Some(rest) = stem.strip_suffix(hint) {
                // 连同紧挨着的分隔点号一起剥掉。否则 `a.[native].md` 会归一成
                // `a.`，而 `a.md` 归一成 `a`，同一个文档被算成两个名字，
                // 409 之后按文件名反查就永远查不到自己。
                stem = rest.trim_end_matches('.');
                stripped = true;
            }
        }
        if !stripped {
            return stem.to_string();
        }
    }
}

pub struct RAGRequest<'a> {
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
        let body = crate::http_util::error_body(response).await;
        if !status.is_success() {
            return Err(format!("登录失败，状态码: {status}，响应: {body}").into());
        }

        let login_response: lightrag_model::LoginResponse = serde_json::from_str(&body)
            .map_err(|e| format!("解析登录响应失败: {e}，响应: {body}"))?;
        // 空 token 会让后续请求以 401 的形式失败，把登录问题伪装成接口问题
        if login_response.access_token.is_empty() {
            return Err("登录响应中 access_token 为空，拒绝继续".into());
        }
        Ok(login_response.access_token)
    }
    async fn _upload(&self, file_path: &str) -> Result<String, UploadError> {
        let upload_url = format!("{}/documents/upload", self.url);
        let file = tokio::fs::File::open(file_path)
            .await
            .map_err(|e| UploadError::fatal(format!("打开文件失败: {e}")))?;
        let stream = FramedRead::new(file, tokio_util::codec::BytesCodec::new());
        let body = reqwest::Body::wrap_stream(stream);
        // file_name 必须给 basename，不能传完整路径：服务端拿 filename 判同名，
        // `/home/a/b.md` 会让它按整条路径当键，判重和落盘目录都会跑偏。
        // mime_str 不用设，服务端只看扩展名（见 lightrag-upload-endpoint-test.md §3.1）。
        let name = basename(file_path);
        let part = reqwest::multipart::Part::stream(body).file_name(name);
        let form = reqwest::multipart::Form::new().part("file", part);
        let resp = self
            .client
            .post(&upload_url)
            .bearer_auth(&self.jwt_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| UploadError::fatal(format!("上传文件请求失败: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::CONFLICT {
            // 409：同名文档已存在。服务端在写文件之前就同步拒绝，并要求先删掉
            // 已有记录。detail 区分了两种来源：文档存储里已有 / input 目录里已有。
            let body = crate::http_util::error_body(resp).await;
            return Err(UploadError::conflict(&body));
        }
        if status.is_success() {
            let resp_json: lightrag_model::UploadResponse = resp
                .json()
                .await
                .map_err(|e| UploadError::fatal(format!("解析上传响应失败: {e}")))?;
            // 枚举是 success / partial_success / failure，只有 success 有 track_id。
            if resp_json.status == "success" {
                Ok(resp_json.track_id)
            } else {
                Err(UploadError::fatal(format!(
                    "上传文件失败，状态码: {status}，响应: {:?}",
                    resp_json.message
                )))
            }
        } else {
            let body = crate::http_util::error_body(resp).await;
            Err(UploadError::fatal(format!(
                "上传文件失败，状态码: {status}，响应: {body}"
            )))
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
                let body = crate::http_util::error_body(resp).await;
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
            delete_llm_cache: true,
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
        let body = crate::http_util::error_body(resp).await;
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

    /// 翻完所有页拉回文档列表。必须翻完：服务端 page_size 上限 200，
    /// 只看第一页会漏判。两处判定（存在性 / 按文件名反查）都走这里，
    /// 免得修一处漏一处。
    async fn fetch_all_documents(&self) -> Result<Vec<lightrag_model::DocStatusResponse>, Box<dyn std::error::Error>> {
        let url = format!("{}/documents/paginated", self.url);
        let mut req = lightrag_model::DocumentsRequest::first_page();
        let mut all = Vec::new();
        for _ in 0..MAX_PAGES {
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
                let body = crate::http_util::error_body(resp).await;
                return Err(format!("查询文档列表失败，状态码: {status}，响应: {body}").into());
            }
            let page: lightrag_model::PaginatedDocsResponse = resp
                .json()
                .await
                .map_err(|e| format!("解析文档列表失败: {e}"))?;
            all.extend(page.documents);
            if !page.pagination.has_next {
                return Ok(all);
            }
            req.page += 1;
        }
        Err(format!(
            "文档列表翻页超过 {MAX_PAGES} 页仍未结束（共取到 {} 条），疑似服务端一直回 has_next",
            all.len()
        )
        .into())
    }

    /// 文档是否还在库里。查 paginated 列表，是判定删除是否生效的唯一权威依据。
    pub async fn document_exists(&self, doc_id: &str) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(self
            .fetch_all_documents()
            .await?
            .iter()
            .any(|d| d.id == doc_id))
    }

    /// 按文件名反查占用它的 doc_id。服务端把文件名当唯一文档键，409 只在
    /// detail 里给文件名，doc_id 得自己从列表里找出来。
    pub async fn find_doc_ids_by_filename(
        &self,
        name: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let target = canonical_basename(name);
        Ok(self
            .fetch_all_documents()
            .await?
            .iter()
            .filter(|doc| canonical_basename(&basename(&doc.file_path)) == target)
            .map(|doc| doc.id.clone())
            .collect())
    }

    /// 清掉占用该文件名的旧文档，让重传能过。
    async fn remove_conflicting(
        &self,
        name: &str,
        detail: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let found = self.find_doc_ids_by_filename(name).await?;
        match found.as_slice() {
            [] => Err(format!(
                "上传 {name} 返回 409 同名冲突，但文档列表里找不到它。\
                 冲突多半来自 input 目录里的残留文件（没有对应索引记录），\
                 delete_document 清不掉这种情况，需要人工确认。服务端原始信息: {detail}"
            )
            .into()),
            [doc_id] => {
                println!("[冲突] {name} 已被 {doc_id} 占用，先删除再重传");
                self.delete_document(doc_id).await
            }
            many => Err(format!(
                "{name} 匹配到多个文档 {many:?}，拒绝自动删除以免误删。服务端原始信息: {detail}"
            )
            .into()),
        }
    }

    /// 上传并等待后台处理完成，返回 doc_id。
    ///
    /// 同名冲突的清理由**这里**做，不放在调用方：调用方手上只有本地路径，
    /// 判不出 409 说的是哪个 doc_id，而 doc_id 必须按文件名回服务端反查。
    /// `RAGRequest` 也不持有 DB 连接，拿不到调用方存的 rag_id。
    pub async fn upload_file(&self, file_path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let name = basename(file_path);
        let track_id = match self._upload(file_path).await {
            Ok(track_id) => track_id,
            // 只重试一次：删完再传还是 409 说明有别的成因，
            // 再循环下去只是把一次失败拖成无数次请求。
            Err(UploadError::Conflict(detail)) => {
                self.remove_conflicting(&name, &detail).await?;
                self._upload(file_path)
                    .await
                    .map_err(|e| e.into_err(&format!("清理同名冲突后重传 {file_path} 仍失败")))?
            }
            Err(e) => return Err(e.into_err(&format!("上传 {file_path} 失败"))),
        };

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
