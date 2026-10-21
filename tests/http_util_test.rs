//! 代理该怎么走：本地地址直连，其余按环境变量走代理。
//!
//! 环境里配了 HTTP_PROXY 时，打向 127.0.0.1 的请求被塞给代理：
//! 多绕一跳，代理抽风还会间歇性 502，整轮同步跟着挂。

mod common;

use common::{CannedResponse as R, MockServer, Route};
use rag_sync::http_util::{ProxySettings, build_client};

/// 本地服务直连：代理指向一个没人监听的端口，走代理的请求必然失败。
#[tokio::test]
async fn loopback_requests_bypass_the_proxy() {
    let server = MockServer::start(vec![Route::get("/ping").and(R::text(200, "pong"))]).await;
    let client = build_client(Some(ProxySettings {
        http: Some("http://127.0.0.1:1".into()),
        https: Some("http://127.0.0.1:1".into()),
        all: None,
    }))
    .unwrap();

    let resp = client.get(format!("{}/ping", server.base_url)).send().await;
    assert!(
        resp.is_ok(),
        "本地地址不该走代理: {:?}",
        resp.err().map(|e| e.to_string())
    );
    assert_eq!(resp.unwrap().text().await.unwrap(), "pong");
}

/// 其余地址按环境变量的规矩走代理：请求会被原样发给代理（绝对形式的 URL）。
#[tokio::test]
async fn other_requests_go_through_the_proxy() {
    let server = MockServer::start(vec![]).await;
    let client = build_client(Some(ProxySettings {
        http: Some(server.base_url.clone()),
        https: None,
        all: None,
    }))
    .unwrap();

    // 代理会收到这条请求（目标主机解析不了也无所谓，代理自己会解析）
    let _ = client.get("http://example.invalid/thing").send().await;

    let proxied = server.requests_to("GET", "http://example.invalid/thing");
    assert_eq!(
        proxied.len(),
        1,
        "非本地地址该走代理。实际收到的请求: {}",
        server.dump()
    );
}

/// 没有代理设置时，客户端照常直连。
#[tokio::test]
async fn without_proxy_settings_requests_go_direct() {
    let server = MockServer::start(vec![Route::get("/ping").and(R::text(200, "pong"))]).await;
    let client = build_client(None).unwrap();

    let resp = client.get(format!("{}/ping", server.base_url)).send().await;
    assert!(resp.is_ok());
    assert_eq!(server.count_to("GET", "/ping"), 1);
}
