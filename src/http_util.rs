//! HTTP 层的公共设施。
//!
//! 只有两块东西，但它们散在三个文件里各写一遍很容易走样：
//! 客户端超时（一个漏设就等于没有防护）与「读错误响应体」的统一格式。

use std::time::Duration;

/// 主机关不到就快速失败，别把一轮同步耗在这上面
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 单次读操作的上限。流式上传时是每个 chunk 各自计时，不会截断大文件。
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// 建一个带超时的 HTTP 客户端。
///
/// reqwest 默认**既不限制连接时间也不限制读取时间**：主机不可达时会挂到
/// 系统给不出答案为止。轮询里的「上限」（MAX_POLL_ROUNDS / TRACK_TIMEOUT）
/// 也只在请求**返回之后**才检查得到，所以它们挡不住一个不回包的服务端。
///
/// 这里只设 connect / read 超时，不设 `timeout()`（整体超时）——
/// `put_content` 是流式上传大文件，整体超时会把它腰斩。
pub fn client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()?;
    Ok(client)
}

/// 读响应体，**只用于错误信息**。
///
/// 空 body 标成 `<空响应>`，读失败标成 `<响应体读取失败: ...>` —— 两者
/// 必须能区分开。原来这里是 `unwrap_or_default()`，读失败与「服务端就是返回空」
/// 在报错里长得一模一样，排查时会照着一个并不存在的空 body 去猜原因。
pub async fn error_body(resp: reqwest::Response) -> String {
    match resp.text().await {
        Ok(body) if body.trim().is_empty() => "<空响应>".into(),
        Ok(body) => body,
        Err(e) => format!("<响应体读取失败: {e}>"),
    }
}
