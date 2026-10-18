#![allow(non_snake_case)]
//! MinerU 的上传 -> 解析 -> 取产物链路，对着 mock 服务端跑。
//!
//! 这条链路有两个坑：契约是 `additionalProperties: false`（字段名写错直接 422），
//! 以及「按 sha256 幂等命中时 upload_url 为 null，再 PUT 会 409」。

mod common;

use common::{CannedResponse as R, MockServer, Route};

use rag_sync::db_model::File;
use rag_sync::mineru_requests::mineru_tackle;

fn 样例文件(dir: &tempfile::TempDir, id: i32, name: &str, content: &str) -> File {
    let p = dir.path().join(name);
    std::fs::write(&p, content).unwrap();
    File {
        id,
        path: p.to_string_lossy().into_owned(),
        sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        rag_id: None,
        run_id: 1,
    }
}

async fn 起(
    dir: &tempfile::TempDir,
    routes: Vec<Route>,
) -> (MockServer, rag_sync::sync_config::SyncConfig) {
    let server = MockServer::start(routes).await;
    let cfg = rag_sync::sync_config::SyncConfig {
        sync_dir: dir.path().to_string_lossy().into_owned(),
        output_dir: dir.path().join("out").to_string_lossy().into_owned(),
        mineru_url: server.base_url.clone(),
        lightrag_url: String::new(),
        lightrag_username: "u".into(),
        lightrag_password: "p".into(),
        run_id: 1,
    };
    (server, cfg)
}

// ---------------------------------------------------------------- 请求体形状

#[test]
fn source_序列化_只留_type_对应的那一个字段() {
    // 契约为 additionalProperties: false，带 null 字段会被 422
    let src = rag_sync::mineru_model::FileSource::from_file_id("f-1".into());
    assert_eq!(
        serde_json::to_string(&src).unwrap(),
        r#"{"type":"file_id","file_id":"f-1"}"#
    );
}

#[test]
fn 上传请求体_字段名与契约一致() {
    let req = rag_sync::mineru_model::FileUploadRequest {
        filename: "a.pdf",
        bytes: 1234,
        mime_type: "application/pdf",
        sha256sum: "deadbeef",
    };
    assert_eq!(
        serde_json::to_string(&req).unwrap(),
        r#"{"filename":"a.pdf","bytes":1234,"mime_type":"application/pdf","sha256sum":"deadbeef"}"#
    );
}

#[test]
fn 解析请求体_是_files_数组包一层() {
    let req = rag_sync::mineru_model::FileParseRequest {
        files: vec![rag_sync::mineru_model::FileParseItem {
            source: rag_sync::mineru_model::FileSource::from_file_id("f-1".into()),
        }],
    };
    assert_eq!(
        serde_json::to_string(&req).unwrap(),
        r#"{"files":[{"source":{"type":"file_id","file_id":"f-1"}}]}"#
    );
}

#[test]
fn 上传响应_幂等命中时_file_为_null() {
    // 按 sha256 幂等命中，upload_url / upload_method / file 全是 null
    let u: rag_sync::mineru_model::UploadObject = serde_json::from_str(
        r#"{"id":"up-1","status":"completed","file":{"id":"f-9"},
            "upload_url":null,"upload_method":null}"#,
    )
    .unwrap();
    assert_eq!(u.status, "completed");
    assert_eq!(u.file.unwrap().id, "f-9");

    let u: rag_sync::mineru_model::UploadObject = serde_json::from_str(
        r#"{"id":"up-2","status":"completed","file":null,"upload_url":null}"#,
    )
    .unwrap();
    assert!(u.file.is_none(), "命中但还没解析出 file 时就是 null");
}

// ---------------------------------------------------------------- 完整上传流程

#[tokio::test]
async fn 新文件走完整上传_上传内容_再完成() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello world");
    let (server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete").and(R::json(
                200,
                r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#,
            )),
            Route::post("/v1/file_parse").and(R::json(
                200,
                r##"{"status":"completed","task_id":"t1","files":[
                     {"name":"a.pdf","status":"done","content":{"markdown":"# 转换结果"}}]}"##,
            )),
        ],
    )
    .await;

    let md = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap();
    assert_eq!(md, "# 转换结果");

    // 创建上传：filename 只给 basename，sha256 直接复用本地算好的
    let create = &server.requests_to("POST", "/v1/uploads")[0];
    let body = create.json();
    assert_eq!(body["filename"], "a.pdf");
    assert_eq!(body["mime_type"], "application/pdf");
    assert_eq!(body["bytes"], 11);
    assert_eq!(body["sha256sum"], file.sha256);

    // 上传内容：声明了长度就不该用 chunked，且字节要完整
    let put = &server.requests_to("PUT", "/v1/uploads/up-1/content")[0];
    assert_eq!(put.header("content-type"), Some("application/pdf"));
    assert_eq!(
        put.header("content-length"),
        Some("11"),
        "声明长度，避免服务端用 chunked"
    );
    assert_eq!(put.body_str(), "hello world", "流式上传不能少字节");

    // 完成后拿 file_id 去解析
    let parse = &server.requests_to("POST", "/v1/file_parse")[0];
    assert_eq!(parse.json()["files"][0]["source"]["file_id"], "f-1");
}

#[tokio::test]
async fn 幂等命中_不重复上传内容() {
    // 服务端已按 sha256 收过这份内容，直接 completed。再 PUT 会得到
    // 409 upload_already_terminal。
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-7"}}"#)),
            Route::put("/v1/uploads/up-1/content").once(),
            Route::post("/v1/file_parse").and(R::json(
                200,
                r#"{"status":"completed","task_id":"t1","files":[
                     {"name":"a.pdf","status":"done","content":{"markdown":"md"}}]}"#,
            )),
        ],
    )
    .await;

    mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap();
    assert_eq!(
        server.count_to("PUT", "/v1/uploads/up-1/content"),
        0,
        "已 completed 还去 PUT 内容，必然 409"
    );
    assert_eq!(
        server.requests_to("POST", "/v1/file_parse")[0].json()["files"][0]["source"]["file_id"],
        "f-7",
        "要直接复用响应里的 file.id"
    );
}

#[tokio::test]
async fn 幂等命中但响应缺_file_报错() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (server, cfg) = 起(
        &d,
        vec![Route::post("/v1/uploads").and(R::json(
            200,
            r#"{"id":"up-1","status":"completed","file":null}"#,
        ))],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("file.id"), "{err}");
    assert_eq!(server.count_to("POST", "/v1/file_parse"), 0);
}

#[tokio::test]
async fn 缺少_sha256_直接拒绝_不浪费一次上传() {
    let d = tempfile::tempdir().unwrap();
    let mut file = 样例文件(&d, 0, "a.pdf", "hello");
    file.sha256 = String::new();
    let (server, cfg) = 起(
        &d,
        vec![Route::post("/v1/uploads").once()],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("sha256"), "{err}");
    assert_eq!(server.count_to("POST", "/v1/uploads"), 0);
}

#[tokio::test]
async fn 文件在磁盘上消失_报错带路径() {
    let d = tempfile::tempdir().unwrap();
    let mut file = 样例文件(&d, 0, "a.pdf", "hello");
    std::fs::remove_file(&file.path).unwrap();
    let (server, cfg) = 起(
        &d,
        vec![Route::post("/v1/uploads").once()],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("a.pdf"), "报错要能定位到是哪个文件: {err}");
    assert_eq!(server.count_to("POST", "/v1/uploads"), 0);
}

#[tokio::test]
async fn 创建上传被拒_报错带文件路径与状态() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (_server, cfg) = 起(
        &d,
        vec![Route::post("/v1/uploads").and(R::json(
            422,
            r#"{"detail":"mime_type unsupported"}"#,
        ))],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("a.pdf"), "{err}");
    assert!(err.contains("422"), "{err}");
}

#[tokio::test]
async fn 完成返回非终态_报错() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (_server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"hashing","file":null}"#)),
        ],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("hashing"), "{err}");
}

// ---------------------------------------------------------------- 解析

#[tokio::test]
async fn 解析_running_轮询到终态后取产物内容() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
            Route::post("/v1/file_parse")
                .and(R::json(202, r#"{"status":"running","task_id":"t1","files":[]}"#)),
            // 轮询终态的 content 恒为空，必须走 output_files 的 file_id 二次取
            Route::get("/v1/tasks/t1").and(R::json(
                200,
                r#"{"status":"completed","task_id":"t1","progress":{"completed":1,"failed":0,"total":1},
                    "files":[{"name":"a.pdf","status":"done",
                              "output_files":{"markdown":{"file_id":"out-1","bytes":8}}}]}"#,
            )),
            Route::get("/v1/files/out-1/content").and(R::text(200, "# 正文\n")),
        ],
    )
    .await;

    let md = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap();
    assert_eq!(md, "# 正文\n");
    assert_eq!(server.count_to("GET", "/v1/tasks/t1"), 1, "一次就轮询到终态");
    assert_eq!(server.count_to("GET", "/v1/files/out-1/content"), 1);
}

#[tokio::test]
async fn 解析_failed_把该文件的错误带出来() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (_server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
            Route::post("/v1/file_parse")
                .and(R::json(202, r#"{"status":"running","task_id":"t1","files":[]}"#)),
            Route::get("/v1/tasks/t1").and(R::json(
                200,
                r#"{"status":"failed","task_id":"t1","files":[
                     {"name":"a.pdf","status":"failed","error":{"message":"OCR 引擎崩了"}}]}"#,
            )),
        ],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("failed"), "{err}");
    assert!(err.contains("OCR 引擎崩了"), "{err}");
}

#[tokio::test]
async fn 解析_cancelled_也要当失败() {
    // 终态有四种，只判 completed 会让失败/取消的任务被当成成功
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (_server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
            Route::post("/v1/file_parse").and(R::json(
                202,
                r#"{"status":"running","task_id":"t1","files":[]}"#,
            )),
            Route::get("/v1/tasks/t1").and(R::json(
                200,
                r#"{"status":"cancelled","task_id":"t1","files":[]}"#,
            )),
        ],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("cancelled"), "{err}");
}

#[tokio::test]
async fn 内联_content_为_null_时回退到产物_id() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
            Route::post("/v1/file_parse").and(R::json(
                200,
                r#"{"status":"completed","task_id":"t1","files":[
                     {"name":"a.pdf","status":"done",
                      "content":{"markdown":null,"images":null},
                      "output_files":{"markdown":{"file_id":"out-2","bytes":4}}}]}"#,
            )),
            Route::get("/v1/files/out-2/content").and(R::text(200, "abcd")),
        ],
    )
    .await;

    assert_eq!(
        mineru_tackle(&reqwest::Client::new(), &file, &cfg).await.unwrap(),
        "abcd"
    );
    assert_eq!(server.count_to("GET", "/v1/files/out-2/content"), 1);
}

#[tokio::test]
async fn 既无内联也无产物_id_报错_不返回空文件() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (_server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
            Route::post("/v1/file_parse").and(R::json(
                200,
                r#"{"status":"completed","task_id":"t1","files":[{"name":"a.pdf","status":"done"}]}"#,
            )),
        ],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("markdown"), "{err}");
}

#[tokio::test]
async fn 解析响应里没有_files_条目_报错() {
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (_server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
            Route::post("/v1/file_parse")
                .and(R::json(200, r#"{"status":"completed","task_id":"t1","files":[]}"#)),
        ],
    )
    .await;

    let err = mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("files"), "{err}");
}

#[tokio::test]
async fn 内容是原样文本_不是_json() {
    // GET /v1/files/{id}/content 直接返回 markdown 原文，
    // 按 JSON 解会得到一个空串再写进输出文件
    let d = tempfile::tempdir().unwrap();
    let file = 样例文件(&d, 0, "a.pdf", "hello");
    let (_server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
            Route::post("/v1/file_parse").and(R::json(
                200,
                r#"{"status":"completed","task_id":"t1","files":[
                     {"name":"a.pdf","status":"done",
                      "output_files":{"markdown":{"file_id":"o","bytes":1}}}]}"#,
            )),
            Route::get("/v1/files/o/content").and(R::text(200, "标题\n\n正文 **加粗**")),
        ],
    )
    .await;

    assert_eq!(
        mineru_tackle(&reqwest::Client::new(), &file, &cfg).await.unwrap(),
        "标题\n\n正文 **加粗**"
    );
}

#[tokio::test]
async fn 大文件按声明长度流式上传() {
    // 不能把整个文件读进内存；这里只验证「声明的长度 == 实际字节数」
    let d = tempfile::tempdir().unwrap();
    let body = "x".repeat(300_000);
    let file = 样例文件(&d, 0, "big.pdf", &body);
    let (server, cfg) = 起(
        &d,
        vec![
            Route::post("/v1/uploads")
                .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
            Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
            Route::post("/v1/uploads/up-1/complete")
                .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
            Route::post("/v1/file_parse").and(R::json(
                200,
                r#"{"status":"completed","task_id":"t1","files":[
                     {"name":"big.pdf","status":"done","content":{"markdown":"ok"}}]}"#,
            )),
        ],
    )
    .await;

    mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap();
    let put = &server.requests_to("PUT", "/v1/uploads/up-1/content")[0];
    assert_eq!(put.header("content-length"), Some("300000"));
    assert_eq!(put.body.len(), 300_000);
    assert_eq!(server.requests_to("POST", "/v1/uploads")[0].json()["bytes"], 300_000);
}
