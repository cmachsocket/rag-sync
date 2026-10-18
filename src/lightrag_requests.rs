use tokio::stream;
use tokio_util::codec::FramedRead;

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
    async fn _upload(&self, file_path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let upload_url = format!("{}/documents/upload", self.url);
        let file = tokio::fs::File::open(file_path)
            .await
            .map_err(|e| format!("打开文件失败: {e}"))?;
        let stream = FramedRead::new(file, tokio_util::codec::BytesCodec::new());
        let body = reqwest::Body::wrap_stream(stream);
        let part = reqwest::multipart::Part::stream(body).file_name(file_path.to_string());
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
            if resp_json.status == "sucess" {
                Ok(resp_json.track_id)
            } else {
                Err(format!(
                    "上传文件失败，状态码: {status}，响应: {:?}",
                    resp_json.message
                )
                .into())
            }
        }
        else{
            let body = resp.text().await.unwrap_or_default();
            Err(format!("上传文件失败，状态码: {status}，响应: {body}").into())
        }
    }
    // 返回file_id
    pub async fn _track(&self, track_id: &str) -> Result<String, Box<dyn std::error::Error>> {
        let track_url = format!("{}/documents/track/{}", self.url, track_id);
        let resp = self
            .client
            .get(&track_url)
            .bearer_auth(&self.jwt_token)
            .send()
            .await
            .map_err(|e| format!("获取文件状态请求失败: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            let resp_json: lightrag_model::TrackResponse = resp
                .json()
                .await
                .map_err(|e| format!("解析获取文件状态响应失败: {e}"))?;
            if resp_json.status == "sucess" {
                Ok(resp_json.id)
            } else {
                Err(format!(
                    "获取文件状态失败，状态码: {status}，响应: {:?}",
                    resp_json
                )
                .into())
            }
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(format!("获取文件状态失败，状态码: {status}，响应: {body}").into())
        }
    }

    pub async fn upload_file(&self, file_path: &str) -> Result<String, Box<dyn std::error::Error>> {
        let track_id = self._upload(file_path).await?;
        let file_id = self._track(&track_id).await?;
        Ok(file_id)
    }
}
