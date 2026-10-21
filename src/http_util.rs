//! HTTP 层的公共设施：客户端超时与「读错误响应体」的统一格式。

use std::time::Duration;

/// 主机关不到就快速失败，别把一轮同步耗在这上面
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 单次读操作的上限。流式上传时是每个 chunk 各自计时，不会截断大文件。
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// 不走代理的地址。本地服务（MinerU 常年跑在 127.0.0.1）打给代理没有意义，
/// 代理抽风时还会把本地请求一起带崩。
const LOCAL_ADDRESSES: &str = "localhost,127.0.0.1,::1,[::1]";

/// 代理设置，按协议分档。没配的那一档就是 `None`。
#[derive(Debug, Clone, Default)]
pub struct ProxySettings {
    /// `http://` 的请求走这个代理。
    pub http: Option<String>,
    /// `https://` 的请求走这个代理。
    pub https: Option<String>,
    /// 两种协议都没单独配时走这个。
    pub all: Option<String>,
}

impl ProxySettings {
    /// 从环境变量读。没配任何代理变量时返回 `None`：
    /// 那就让 reqwest 用它自己的系统代理逻辑。
    pub fn from_env() -> Option<Self> {
        let settings = Self {
            http: env_var("HTTP_PROXY"),
            https: env_var("HTTPS_PROXY"),
            all: env_var("ALL_PROXY"),
        };
        if settings.is_empty() {
            None
        } else {
            Some(settings)
        }
    }

    fn is_empty(&self) -> bool {
        self.http.is_none() && self.https.is_none() && self.all.is_none()
    }
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .or_else(|| std::env::var(key.to_ascii_lowercase()).ok())
        .filter(|value| !value.trim().is_empty())
}

/// 排除名单：环境的 `NO_PROXY` 加上本地地址。
fn exclusion_list() -> String {
    let mut list = LOCAL_ADDRESSES.to_string();
    if let Some(env) = env_var("NO_PROXY") {
        list.push(',');
        list.push_str(&env);
    }
    list
}

/// 建一个带超时的 HTTP 客户端。reqwest 默认不限制连接与读取时间，
/// 轮询里的超时又只在请求返回之后才检查得到，挡不住不回包的服务端。
/// 不设整体 `timeout()`：`put_content` 是流式上传大文件。
///
/// 代理按环境的规矩来（`HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`），
/// 只是把本地地址加进排除名单。
pub fn client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    build_client(ProxySettings::from_env())
}

/// 按给定的代理设置建客户端。`settings` 为 `None` 表示不走代理。
///
/// 显式设过代理之后 reqwest 就不再自动用「系统代理」了，所以这里要自己把
/// 那套规矩实现一遍：`HTTPS_PROXY` 管 https，`HTTP_PROXY` 管 http，
/// `ALL_PROXY` 兜底，`NO_PROXY` 排除。
pub fn build_client(
    settings: Option<ProxySettings>,
) -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT);
    if let Some(settings) = settings {
        let no_proxy = reqwest::NoProxy::from_string(&exclusion_list());
        if let Some(proxy) = settings.https {
            builder = builder.proxy(reqwest::Proxy::https(proxy)?.no_proxy(no_proxy.clone()));
        }
        if let Some(proxy) = settings.http {
            builder = builder.proxy(reqwest::Proxy::http(proxy)?.no_proxy(no_proxy.clone()));
        }
        if let Some(proxy) = settings.all {
            builder = builder.proxy(reqwest::Proxy::all(proxy)?.no_proxy(no_proxy));
        }
    }
    let client = builder.build()?;
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
