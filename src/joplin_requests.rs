//! Joplin Server 的登录、delta 同步流与文件内容。
//!
//! 同步流用服务端给的 cursor 翻页：cursor 可能指向一条被压缩掉、从未下发给客户端的
//! 事件，只能原样传回去，不能自己拿「最后一条返回事件的 id」当游标。

use crate::http_util;
use crate::joplin_model;
use std::fmt;
use std::time::Duration;

/// 单次请求的超时。取内容要下整份文件，给得比一般请求宽。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// delta 每页的默认条数。
pub const DEFAULT_LIMIT: usize = 100;

/// 一轮同步最多翻多少页：只防服务端一直回 `has_more`。
pub const MAX_PAGES: usize = 10_000;

/// 取文件内容的路径前缀。路径形如 `root:/<文件名>:`，末尾的冒号不能少。
const ITEMS_PATH: &str = "/api/items";

/// 登录接口。v3 的会话接口是 `POST /api/sessions`，返回的 id 就是要带的 cookie 值。
const SESSIONS_PATH: &str = "/api/sessions";

/// 根目录的 delta 路径。delta 只有全局一档，按目录做增量同步不成立。
const ROOT_DELTA_PATH: &str = "/api/items/root:/:/delta";

/// Joplin Server 返回的错误。
#[derive(Debug)]
pub enum JoplinError {
    /// 游标失效：服务端要求丢掉游标、从头全量同步。
    ResyncRequired,
    /// 文件已经不在了。
    NotFound,
    /// 其他错误，带上可读的原因。
    Other(String),
}

impl fmt::Display for JoplinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JoplinError::ResyncRequired => write!(f, "delta 游标失效，需要从头全量同步"),
            JoplinError::NotFound => write!(f, "文件不存在"),
            JoplinError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for JoplinError {}

/// 一页 delta 结果。
#[derive(Debug)]
pub struct DeltaPage {
    pub items: Vec<joplin_model::JoplinSyncItem>,
    /// 服务端给的游标，原样传回去翻下一页。
    pub cursor: Option<String>,
    /// 还有没有下一页。翻页只能按它停。
    pub has_more: bool,
}

/// 已登录的 Joplin Server 客户端。
pub struct JoplinClient {
    client: reqwest::Client,
    /// 会话 id，请求时作为 `sessionId` cookie 带上。
    session_id: String,
    url: String,
}

impl JoplinClient {
    /// 登录并建客户端。登录失败直接返回 Err：没有会话的话每个请求都是 403。
    pub async fn new(
        client: &reqwest::Client,
        url: &str,
        email: &str,
        password: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let session_id = Self::login(client, url, email, password).await?;
        Ok(JoplinClient {
            client: client.clone(),
            session_id,
            url: url.trim_end_matches('/').to_string(),
        })
    }

    async fn login(
        client: &reqwest::Client,
        url: &str,
        email: &str,
        password: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let req = joplin_model::JoplinLoginRequest {
            email: email.to_string(),
            password: password.to_string(),
        };
        let login_url = format!("{}{SESSIONS_PATH}", url.trim_end_matches('/'));
        let resp = tokio::time::timeout(REQUEST_TIMEOUT, client.post(&login_url).json(&req).send())
            .await
            .map_err(|_| format!("登录请求超时（{REQUEST_TIMEOUT:?}）: {login_url}"))?
            .map_err(|e| format!("登录请求失败: {e}"))?;

        let status = resp.status();
        let body = http_util::error_body(resp).await;
        if !status.is_success() {
            return Err(format!("登录失败，状态码: {status}，响应: {body}").into());
        }
        let login_response: joplin_model::JoplinLoginResponse = serde_json::from_str(&body)
            .map_err(|e| format!("解析登录响应失败: {e}，响应: {body}"))?;
        if login_response.id.is_empty() {
            return Err("登录响应里没有会话 id".into());
        }
        Ok(login_response.id)
    }

    /// 取一页变更事件。`cursor` 为 None 表示从头开始。
    pub async fn delta(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DeltaPage, JoplinError> {
        let mut query: Vec<(&str, String)> = vec![("limit", limit.max(1).to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        let url = format!("{}{ROOT_DELTA_PATH}", self.url);
        let resp = self
            .client
            .get(&url)
            .query(&query)
            .header("Cookie", format!("sessionId={}", self.session_id))
            .send()
            .await
            .map_err(|e| JoplinError::Other(format!("同步请求失败: {e}")))?;

        let status = resp.status();
        let body = http_util::error_body(resp).await;
        if !status.is_success() {
            // 游标失效是服务端明确给的说法，要单独认出来
            if let Ok(err) = serde_json::from_str::<joplin_model::JoplinErrorResponse>(&body)
                && err.code.as_deref() == Some("resyncRequired")
            {
                return Err(JoplinError::ResyncRequired);
            }
            return Err(JoplinError::Other(format!(
                "同步失败，状态码: {status}，响应: {body}"
            )));
        }
        let page: joplin_model::JoplinSyncResponse = serde_json::from_str(&body)
            .map_err(|e| JoplinError::Other(format!("解析同步响应失败: {e}，响应: {body}")))?;
        Ok(DeltaPage {
            items: page.items,
            cursor: page.cursor,
            has_more: page.has_more,
        })
    }

    /// 取一个文件的内容（正文 + 末尾的元信息块）。
    /// 文件已经不在了时返回 [`JoplinError::NotFound`]。
    pub async fn content(&self, item_name: &str) -> Result<String, JoplinError> {
        // 路径形如 `root:/<文件名>:`，文件名里带 `/` 或 `:` 会把路径拆错
        if item_name.contains('/') || item_name.contains(':') {
            return Err(JoplinError::Other(format!(
                "文件名 {item_name:?} 不能用于拼路径"
            )));
        }
        let url = format!("{}{ITEMS_PATH}/root:/{item_name}:/content", self.url);
        let resp = self
            .client
            .get(&url)
            .header("Cookie", format!("sessionId={}", self.session_id))
            .send()
            .await
            .map_err(|e| JoplinError::Other(format!("取文件内容失败: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(JoplinError::NotFound);
        }
        if !status.is_success() {
            let body = http_util::error_body(resp).await;
            return Err(JoplinError::Other(format!(
                "取文件内容失败，状态码: {status}，响应: {body}"
            )));
        }
        // 内容是原文，不走 error_body：空正文会被它换成「<空响应>」
        let text = resp
            .text()
            .await
            .map_err(|e| JoplinError::Other(format!("读取文件内容失败: {e}")))?;
        Ok(text)
    }
}
