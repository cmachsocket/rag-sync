use crate::lightrag_model;
use std::time::Duration;

/// 登录是启动路径上的单次交互，卡住应快速失败。
/// reqwest 的 Client 默认无超时，不加这层保护，主机不可达时 new() 会无限期挂起。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10);

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
}
