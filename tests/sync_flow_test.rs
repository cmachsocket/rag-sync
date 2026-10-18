//! create_output_files 的端到端链路：扫描 -> MinerU 转换 -> 写产物 ->
//! 上传 LightRAG -> 落库。这一段改错了不会报错，只会几轮后表现为
//! 「文档没更新」或「原件不见了」。

mod common;

use common::{CannedResponse as R, Env, MockServer, Route};
use rag_sync::database::DBfunc;
use rag_sync::sync_folder::{create_output_files, get_files};

fn rag_login_ok() -> Route {
    Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#))
}

fn upload_ok() -> Route {
    Route::post("/documents/upload")
        .and(R::json(200, r#"{"status":"success","message":"ok","track_id":"tr-1"}"#))
}

fn track_terminal(doc_id: &str) -> Route {
    Route::get("/documents/track_status/tr-1").and(R::json(
        200,
        format!(
            r#"{{"track_id":"tr-1","total_count":1,"status_summary":{{"processed":1}},
                 "documents":[{{"id":"{doc_id}","status":"processed","file_path":"a.pdf"}}]}}"#
        ),
    ))
}

fn mineru_routes(markdown: &str) -> Vec<Route> {
    vec![
        Route::post("/v1/uploads")
            .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
        Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
        Route::post("/v1/uploads/up-1/complete")
            .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
        Route::post("/v1/file_parse").and(R::json(
            200,
            format!(
                r#"{{"status":"completed","task_id":"t1","files":[
                     {{"name":"a.pdf","status":"done","content":{{"markdown":{}}}}}]}}"#,
                serde_json::Value::String(markdown.to_string())
            ),
        )),
    ]
}

/// 起一套完整环境：临时 sync/out 目录 + MinerU + LightRAG。
async fn env_with_services(mineru_md: &str) -> (Env, MockServer) {
    let mut env = Env::new();
    let mut routes = mineru_routes(mineru_md);
    routes.push(rag_login_ok());
    routes.push(upload_ok());
    routes.push(track_terminal("doc-1"));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    (env, server)
}

#[tokio::test]
async fn new_file_end_to_end() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 标题\n\n正文").await;
    let src = env.write_source("a.pdf", "pdf 内容");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let out = create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap();

    assert_eq!(out.len(), 1);
    assert_eq!(env.output_text("a.pdf"), "# 标题\n\n正文", "产物内容应原样落盘");
    assert!(out[0].starts_with(&env.output_dir));

    // 上传给 LightRAG 的就是刚写出来的产物文件
    let upload = server.requests_to("POST", "/documents/upload");
    assert_eq!(upload.len(), 1);
    assert!(
        upload[0].body_str().contains(r#"filename="a.pdf""#),
        "{}",
        upload[0].body_str()
    );
    assert_eq!(server.count_to("GET", "/documents/track_status/tr-1"), 1);

    // 落库：sha256 是源文件的，rag_id 是 LightRAG 的 doc_id
    let row = DBfunc::get_file(&mut db.conn, &src).await.unwrap().unwrap();
    assert_eq!(
        row.sha256,
        rag_sync::sync_folder::sha256_file(&src).await.unwrap()
    );
    assert_eq!(row.rag_id.as_deref(), Some("doc-1"));
}

#[tokio::test]
async fn nested_layout_preserved_in_output() {
    let mut db = common::test_db().await;
    let (env, _s) = env_with_services("md").await;
    env.write_source("2024/一季度/a.pdf", "x");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap();

    assert!(env.output_path("2024/一季度/a.pdf").exists());
}

#[tokio::test]
async fn unchanged_file_is_skipped() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 新内容").await;
    let src = env.write_source("a.pdf", "pdf 内容");
    let sha = rag_sync::sync_folder::sha256_file(&src).await.unwrap();
    db.insert_file_row(1, &src, &sha, 1, Some("doc-old")).await;

    let files = get_files(&env.config(2), &mut db.conn).await.unwrap();
    let out = create_output_files(&env.config(2), &files, &mut db.conn)
        .await
        .unwrap();

    assert!(out.is_empty(), "没变就不该产生新的产物");
    assert_eq!(server.count_to("POST", "/v1/uploads"), 0, "不该重跑 MinerU");
    assert_eq!(server.count_to("POST", "/documents/upload"), 0, "不该重传 LightRAG");
}

#[tokio::test]
async fn mineru_failure_writes_nothing() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let server = MockServer::start(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/v1/uploads").and(R::json(
            500,
            r#"{"detail":"解析器不可用"}"#,
        )),
    ])
    .await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    let src = env.write_source("a.pdf", "pdf 内容");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let err = create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("a.pdf"), "{err}");
    assert!(!env.output_path("a.pdf").exists(), "失败时不该留下空产物");
    assert!(
        !DBfunc::file_exists(&mut db.conn, &src).await.unwrap(),
        "失败的文件不能记成已入库"
    );
}

#[tokio::test]
async fn rag_upload_failure_records_nothing() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let mut routes = mineru_routes("# 标题");
    routes.push(Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)));
    routes.push(
        Route::post("/documents/upload")
            .and(R::json(200, r#"{"status":"failure","message":"知识库不可写","track_id":""}"#)),
    );
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    let src = env.write_source("a.pdf", "pdf 内容");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let err = create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("知识库不可写"), "{err}");
    assert!(
        !DBfunc::file_exists(&mut db.conn, &src).await.unwrap(),
        "LightRAG 没成功就不能记成已完成"
    );
}

#[tokio::test]
async fn rag_login_failure_fails_fast() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let mut routes = mineru_routes("# 标题");
    routes.push(
        Route::post("/login").and(R::json(401, r#"{"detail":"Incorrect username or password"}"#)),
    );
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    env.write_source("a.pdf", "pdf 内容");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let err = create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Incorrect username"), "{err}");
    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        0,
        "登录在循环外，登录失败就不该开始转换"
    );
}

#[tokio::test]
async fn one_failure_is_not_swallowed() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let server = MockServer::start(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/v1/uploads").and(R::json(
            200,
            r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#,
        )),
        Route::post("/v1/file_parse").and(R::json(
            500,
            r#"{"detail":"boom"}"#,
        )),
        Route::post("/documents/upload").and(R::json(
            200,
            r#"{"status":"success","message":"ok","track_id":"tr-1"}"#,
        )),
        Route::get("/documents/track_status/tr-1").and(R::json(
            200,
            r#"{"track_id":"tr-1","total_count":1,"status_summary":{"processed":1},
                "documents":[{"id":"doc-1","status":"processed","file_path":"x.md"}]}"#,
        )),
    ])
    .await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    env.write_source("a.pdf", "x");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let res = create_output_files(&env.config(1), &files, &mut db.conn).await;

    assert!(res.is_err(), "单个文件失败必须冒泡，不能当成功收尾");
}
