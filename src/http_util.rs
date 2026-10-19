//! HTTP 层的公共设施：客户端超时与「读错误响应体」的统一格式。

use std::time::Duration;

/// 主机关不到就快速失败，别把一轮同步耗在这上面
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 单次读操作的上限。流式上传时是每个 chunk 各自计时，不会截断大文件。
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// 建一个带超时的 HTTP 客户端。
///
/// reqwest 默认不限制连接与读取时间，而轮询里的上限只在请求返回之后才检查得到，
/// 挡不住不回包的服务端。不设整体 `timeout()`：`put_content` 是流式上传大文件。
pub fn client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()?;
    Ok(client)
}

/// 读响应体，**只用于错误信息**。空 body 与读取失败必须标成不同文案，
/// 否则排查时会照着一个并不存在的空 body 去猜原因。
pub async fn error_body(resp: reqwest::Response) -> String {
    match resp.text().await {
        Ok(body) if body.trim().is_empty() => "<空响应>".into(),
        Ok(body) => body,
        Err(e) => format!("<响应体读取失败: {e}>"),
    }
}
