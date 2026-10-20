//! create_output_files 端到端链路：扫描 -> MinerU 转换 -> 写产物 -> 上传 LightRAG -> 落库。

mod common;

use common::{CannedResponse as R, Env, MockServer, Route};
use rag_sync::database::DBfunc;
use rag_sync::sync_folder::{
    RetryPolicy, create_output_files, create_output_files_with_retry, get_files,
};
use std::time::Duration;

/// 5 次尝试、不退避：失败用例看的是重试语义，用默认退避会白等 20 秒。
fn no_backoff() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 5,
        base_delay: Duration::ZERO,
    }
}

fn rag_login_ok() -> Route {
    Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#))
}

fn upload_ok() -> Route {
    Route::post("/documents/upload").and(R::json(
        200,
        r#"{"status":"success","message":"ok","track_id":"tr-1"}"#,
    ))
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
    return vec![
        Route::post("/v1/uploads").and(R::json(
            201,
            r#"{"id":"up-1","status":"created","file":null}"#,
        )),
        Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
        Route::post("/v1/uploads/up-1/complete").and(R::json(
            200,
            r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#,
        )),
        // 任务接口：建任务恒 202 queued，正文不内联，要按产物 id 再取一次
        Route::post("/v1/parse/jobs").and(R::json(
            202,
            r#"{"job_id":"j-1","status":"queued","files":[]}"#,
        )),
        Route::get("/v1/parse/jobs/j-1").and(R::json(
            200,
            r#"{"job_id":"j-1","status":"completed","files":[
                 {"name":"a.pdf","status":"completed",
                  "output_files":{"markdown":{"file_id":"out-1","bytes":8}}}]}"#,
        )),
        Route::get("/v1/files/out-1/content").and(R::text(200, markdown)),
    ];
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
    assert_eq!(
        env.output_text("a.pdf.md"),
        "# 标题\n\n正文",
        "产物内容应原样落盘"
    );
    assert!(out[0].starts_with(&env.output_dir));

    // 产物带 .md：LightRAG 按扩展名选解析器，装 markdown 的文件带 .docx 会判成无效文档
    let upload = server.requests_to("POST", "/documents/upload");
    assert_eq!(upload.len(), 1);
    assert!(
        upload[0]
            .body_str()
            .contains(r#"filename="sync__a.pdf.md""#),
        "{}",
        upload[0].body_str()
    );
    assert_eq!(server.count_to("GET", "/documents/track_status/tr-1"), 1);

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

    assert!(env.output_path("2024/一季度/a.pdf.md").exists());
}

/// 两个别名下的同名文件要分开落盘、分开命名，否则第二个 409 时自愈会删掉第一个。
#[tokio::test]
async fn same_name_in_two_aliases_stays_separate() {
    let mut db = common::test_db().await;
    let (mut env, server) = env_with_services("# 标题").await;
    env.write_source("2024/报告.pdf", "工作版");
    env.add_folder("personal");
    env.write_source_in("personal", "2024/报告.pdf", "私人版");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    assert_eq!(files.len(), 2);
    create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap();

    assert_eq!(env.output_text("2024/报告.pdf.md"), "# 标题");
    assert_eq!(
        env.output_text_in("personal", "2024/报告.pdf.md"),
        "# 标题",
        "两个别名下的产物要各自存在，不能互相覆盖"
    );

    let uploaded: Vec<String> = server
        .requests_to("POST", "/documents/upload")
        .iter()
        .map(|r| {
            let b = r.body_str();
            let s = b.find("filename=\"").unwrap() + "filename=\"".len();
            b[s..b[s..].find('"').unwrap() + s].to_string()
        })
        .collect();
    assert_eq!(uploaded.len(), 2, "{uploaded:?}");
    assert!(
        uploaded.contains(&"sync__报告.pdf.md".to_string()),
        "{uploaded:?}"
    );
    assert!(
        uploaded.contains(&"personal__报告.pdf.md".to_string()),
        "{uploaded:?}"
    );
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        0,
        "两个同名文件不该触发 409 自愈"
    );
}

/// 别名改了产物路径就变了：sha256 相同也必须重新处理，否则留下永远对不上的产物。
#[tokio::test]
async fn changed_artifact_path_forces_reprocessing() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 标题").await;
    let src = env.write_source("a.pdf", "pdf 内容");
    let sha = rag_sync::sync_folder::sha256_file(&src).await.unwrap();

    // 上一轮的产物路径记的是别的别名
    db.insert_file_row_full(1, &src, &sha, 1, Some("doc-old"), Some("work/a.pdf.md"))
        .await;

    let files = get_files(&env.config(2), &mut db.conn).await.unwrap();
    create_output_files(&env.config(2), &files, &mut db.conn)
        .await
        .unwrap();

    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        1,
        "产物路径变了却跳过，重新上传的文档永远对不上产物"
    );
    assert_eq!(env.output_text("a.pdf.md"), "# 标题");
    let row = DBfunc::get_file(&mut db.conn, &src).await.unwrap().unwrap();
    assert_eq!(
        row.output_rel_path.as_deref(),
        Some("sync/a.pdf.md"),
        "产物路径要跟着更新，否则下一轮还会当成变了"
    );
}

/// 产物路径在第一次落库时就要写进去，写晚了那一轮的产物就失去关联。
#[tokio::test]
async fn artifact_path_is_recorded_on_first_insert() {
    let mut db = common::test_db().await;
    let (env, _s) = env_with_services("md").await;
    let src = env.write_source("2024/一季度/a.pdf", "x");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap();

    let row = DBfunc::get_file(&mut db.conn, &src).await.unwrap().unwrap();
    assert_eq!(
        row.output_rel_path.as_deref(),
        Some("sync/2024/一季度/a.pdf.md")
    );
}

#[tokio::test]
async fn txt_md_and_markdown_are_passed_through() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# MinerU 的产物").await;
    let md = "# 标题\n\n正文 **加粗**\n";
    let txt = "纯文本第一行\n纯文本第二行\n";
    let markdown = "### 三级标题\n";
    env.write_source("a.md", md);
    env.write_source("b.txt", txt);
    env.write_source("c.markdown", markdown);
    env.write_source("d.pdf", "pdf 内容");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap();

    // 产物内容与原文逐字一致，没有经过 MinerU；pdf 照旧走 MinerU
    assert_eq!(env.output_text("a.md"), md);
    assert_eq!(env.output_text("b.txt.md"), txt);
    assert_eq!(env.output_text("c.markdown.md"), markdown);
    assert_eq!(env.output_text("d.pdf.md"), "# MinerU 的产物");

    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        1,
        "txt / md / markdown 不该发往 MinerU"
    );
    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        4,
        "透传的文件照样要进知识库"
    );
    let uploaded: Vec<String> = server
        .requests_to("POST", "/documents/upload")
        .iter()
        .map(|r| {
            let b = r.body_str();
            let s = b.find("filename=\"").unwrap() + "filename=\"".len();
            b[s..b[s..].find('"').unwrap() + s].to_string()
        })
        .collect();
    assert!(uploaded.contains(&"sync__a.md".to_string()), "{uploaded:?}");
    assert!(
        uploaded.contains(&"sync__b.txt.md".to_string()),
        "{uploaded:?}"
    );
    assert!(
        uploaded.contains(&"sync__c.markdown.md".to_string()),
        "{uploaded:?}"
    );

    let src = env.write_source("a.md", md);
    let row = DBfunc::get_file(&mut db.conn, &src).await.unwrap().unwrap();
    assert_eq!(row.rag_id.as_deref(), Some("doc-1"));
}

#[tokio::test]
async fn passthrough_is_case_insensitive() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("x").await;
    let body = "大写扩展名\n";
    env.write_source("NOTES.TXT", body);

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    assert_eq!(files.len(), 1, "大写扩展名也要被扫到");

    create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap();

    assert_eq!(env.output_text("NOTES.txt.md"), body);
    assert_eq!(server.count_to("POST", "/v1/uploads"), 0);
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
    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        0,
        "不该重传 LightRAG"
    );
}

#[tokio::test]
async fn mineru_failure_writes_nothing() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let server = MockServer::start(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/v1/uploads").and(R::json(500, r#"{"detail":"解析器不可用"}"#)),
    ])
    .await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    let src = env.write_source("a.pdf", "pdf 内容");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let err = create_output_files_with_retry(&env.config(1), &files, &mut db.conn, no_backoff())
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("a.pdf"), "{err}");
    assert!(err.contains("尝试 5 次"), "耗尽次数要把次数说清楚: {err}");
    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        5,
        "每个文件失败要试满 5 次"
    );
    assert!(
        !env.output_path("a.pdf.md").exists(),
        "失败时不该留下空产物"
    );
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
    routes.push(Route::post("/documents/upload").and(R::json(
        200,
        r#"{"status":"failure","message":"知识库不可写","track_id":""}"#,
    )));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    let src = env.write_source("a.pdf", "pdf 内容");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let err = create_output_files_with_retry(&env.config(1), &files, &mut db.conn, no_backoff())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("知识库不可写"), "{err}");
    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        5,
        "上传失败要重试 5 次"
    );
    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        1,
        "两步分开算失败：上传失败不该把解析再跑一遍（大文件解析一次几十分钟）"
    );
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
    routes.push(Route::post("/login").and(R::json(
        401,
        r#"{"detail":"Incorrect username or password"}"#,
    )));
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
        Route::post("/v1/parse/jobs").and(R::json(500, r#"{"detail":"boom"}"#)),
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
    let res =
        create_output_files_with_retry(&env.config(1), &files, &mut db.conn, no_backoff()).await;

    assert!(res.is_err(), "单个文件失败必须冒泡，不能当成功收尾");
    assert_eq!(
        server.count_to("POST", "/v1/parse/jobs"),
        5,
        "冒泡之前要先把 5 次机会用完"
    );
}

/// 第 3 次成功要当成正常完成：落盘、入库、记 run_id，一个都不能少。
#[tokio::test]
async fn transient_failure_is_retried_until_success() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let mut routes = mineru_routes("# 标题");
    // 队列按顺序消费：500 -> 500 -> 201，正好对上前两次失败、第三次成功
    routes[0] = Route::post("/v1/uploads")
        .and(R::json(500, r#"{"detail":"解析器抖动"}"#))
        .and(R::json(500, r#"{"detail":"解析器抖动"}"#))
        .and(R::json(
            201,
            r#"{"id":"up-1","status":"created","file":null}"#,
        ));
    routes.push(rag_login_ok());
    routes.push(upload_ok());
    routes.push(track_terminal("doc-1"));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    let src = env.write_source("a.pdf", "pdf 内容");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let out = create_output_files_with_retry(&env.config(1), &files, &mut db.conn, no_backoff())
        .await
        .unwrap();

    assert_eq!(out.len(), 1, "重试成功也算成功");
    assert_eq!(env.output_text("a.pdf.md"), "# 标题");
    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        3,
        "两次失败 + 一次成功"
    );
    let row = DBfunc::get_file(&mut db.conn, &src).await.unwrap().unwrap();
    assert_eq!(
        row.rag_id.as_deref(),
        Some("doc-1"),
        "成功那次的 doc_id 要落库"
    );
}

/// 耗尽 5 次立刻中止整轮：同批后面排队的文件本轮不许开始。
#[tokio::test]
async fn retries_exhausted_aborts_the_run_and_skips_remaining_files() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let server = MockServer::start(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/v1/uploads").and(R::json(500, r#"{"detail":"解析器不可用"}"#)),
    ])
    .await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    // a.pdf 排在 b.pdf 前面（get_files 按路径排序），失败的是 a
    env.write_source("a.pdf", "x");
    env.write_source("b.pdf", "y");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    assert_eq!(files.len(), 2);
    let err = create_output_files_with_retry(&env.config(1), &files, &mut db.conn, no_backoff())
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("a.pdf"), "{err}");
    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        5,
        "只有 a.pdf 的 5 次，b.pdf 一次都不许发"
    );
    assert!(
        !env.output_path("a.pdf.md").exists() && !env.output_path("b.pdf.md").exists(),
        "中止时不许留下任何产物"
    );
}

/// 生产节奏钉在这里：5 次尝试、2s/4s/6s/8s 递增退避，改常量必须先改这个用例。
#[test]
fn default_retry_policy_is_five_attempts_with_growing_backoff() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_attempts, 5, "每个文件 5 次尝试");

    let mut wait = Duration::ZERO;
    for attempt in 1..policy.max_attempts {
        wait += policy.base_delay * attempt;
    }
    assert_eq!(policy.base_delay, Duration::from_secs(2), "退避基数 2 秒");
    assert_eq!(
        wait,
        Duration::from_secs(2 + 4 + 6 + 8),
        "四次等待合计 20 秒"
    );
}

// 两步流水线：转换（透传 / MinerU）与上传（LightRAG）要能同时跑 ——
// 只断言「最后都成功了」看不出有没有并行。

/// 上传卡在服务端不放时，下一个文件应该已经在解析了。
/// 串行实现里第二个文件的解析要等第一个传完才开始，那时只有 1 次 /v1/uploads。
#[tokio::test]
async fn mineru_keeps_working_while_lightrag_is_still_uploading() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let gate = common::Gate::new();
    let mut routes = mineru_routes("# 标题");
    routes.push(rag_login_ok());
    // 上传的响应被闸门扣住：客户端正等着它
    routes.push(
        Route::post("/documents/upload").and(
            R::json(
                200,
                r#"{"status":"success","message":"ok","track_id":"tr-1"}"#,
            )
            .gated(gate.clone()),
        ),
    );
    routes.push(track_terminal("doc-1"));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    env.write_source("a.pdf", "a 的内容");
    env.write_source("b.pdf", "b 的内容");

    let cfg = env.config(1);
    let files = get_files(&cfg, &mut db.conn).await.unwrap();
    let mut sync = Box::pin(create_output_files(&cfg, &files, &mut db.conn));

    // 同步在跑，主协程去观察它有没有被上传那一步卡住
    let parsed = tokio::select! {
        r = &mut sync => panic!("同步不该在上传卡住的时候就结束: {r:?}"),
        n = server.wait_for_count("POST", "/v1/uploads", 2, Duration::from_secs(10)) => n,
    };
    assert_eq!(
        parsed, 2,
        "上传还没回包，第二个文件就该开始解析了 —— 两步没并行的话这里只有 1 次"
    );

    gate.open();
    let out = sync.await.unwrap();
    assert_eq!(out.len(), 2, "两个文件都要走完流水线");
    assert!(env.output_path("a.pdf.md").exists());
    assert!(env.output_path("b.pdf.md").exists());
}

/// 传完的产物路径要按待处理列表的顺序返回，不按谁先传完。
#[tokio::test]
async fn artifact_order_follows_the_todo_list() {
    let mut db = common::test_db().await;
    let (env, _s) = env_with_services("# 标题").await;
    for name in ["a.pdf", "b.pdf", "c.pdf"] {
        env.write_source(name, name);
    }

    let cfg = env.config(1);
    let files = get_files(&cfg, &mut db.conn).await.unwrap();
    let out = create_output_files(&cfg, &files, &mut db.conn)
        .await
        .unwrap();

    let names: Vec<String> = out
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["a.pdf.md", "b.pdf.md", "c.pdf.md"],
        "返回顺序要可复现，不能跟着谁先传完走"
    );
}

/// `lightrag_concurrency` 调到几，就该同时传几份。
/// 闸门把上传响应扣住：并发是 2 时两份都在飞，闸门一开才一起放行。
#[tokio::test]
async fn lightrag_concurrency_controls_how_many_uploads_are_in_flight() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    env.with_concurrency(1, 2);
    let gate = common::Gate::new();
    let mut routes = mineru_routes("# 标题");
    routes.push(rag_login_ok());
    routes.push(
        Route::post("/documents/upload").and(
            R::json(
                200,
                r#"{"status":"success","message":"ok","track_id":"tr-1"}"#,
            )
            .gated(gate.clone()),
        ),
    );
    routes.push(track_terminal("doc-1"));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    env.write_source("a.pdf", "a 的内容");
    env.write_source("b.pdf", "b 的内容");

    let cfg = env.config(1);
    let files = get_files(&cfg, &mut db.conn).await.unwrap();
    let mut sync = Box::pin(create_output_files(&cfg, &files, &mut db.conn));

    let uploaded = tokio::select! {
        r = &mut sync => panic!("同步不该在上传卡住的时候就结束: {r:?}"),
        n = server.wait_for_count("POST", "/documents/upload", 2, Duration::from_secs(10)) => n,
    };
    assert_eq!(
        uploaded, 2,
        "并发调到 2 就该有两份在飞：调了没反应等于没这个配置"
    );

    gate.open();
    let out = sync.await.unwrap();
    assert_eq!(out.len(), 2);
}

/// 上传失败不许牵动解析：解析跑一次就好，重传时产物已经在磁盘上了。
#[tokio::test]
async fn upload_retry_does_not_reparse() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let mut routes = mineru_routes("# 标题");
    routes.push(rag_login_ok());
    routes.push(Route::post("/documents/upload").and(R::json(
        200,
        r#"{"status":"failure","message":"知识库不可写","track_id":""}"#,
    )));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    env.write_source("a.pdf", "pdf 内容");

    let cfg = env.config(1);
    let files = get_files(&cfg, &mut db.conn).await.unwrap();
    let err = create_output_files_with_retry(&cfg, &files, &mut db.conn, no_backoff())
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("知识库不可写"), "{err}");
    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        5,
        "上传自己重试 5 次"
    );
    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        1,
        "解析不该跟着重跑：一次解析几十分钟，不能为了重传再来一遍"
    );
    assert!(
        env.output_path("a.pdf.md").exists(),
        "产物已经落盘了，重传用的就是它"
    );
}

// 同名冲突：LightRAG 的唯一键是文件名，同名就 409。库里记着「上次传的是哪份内容」，
// 才能分清「内容没变」和「内容变了」。

/// 内容没变的同名冲突：不删、不重传，直接沿用知识库里那一份 ——
/// 删了重传会让文档短暂缺席、重新抽一遍，传失败还会把旧文档弄丢。
#[tokio::test]
async fn unchanged_content_resolves_conflict_without_reupload() {
    let mut db = common::test_db().await;
    let (env, server) = conflict_env("# 标题").await;
    let src = env.write_source("a.pdf", "改了内容但产物一样");

    // 上一轮传过：库里记着 doc-1 和当时那份内容的指纹
    db.insert_file_row_full(
        1,
        &src,
        "旧的源文件指纹",
        1,
        Some("doc-1"),
        Some("sync/a.pdf.md"),
    )
    .await;
    db.set_uploaded_sha256(&src, &rag_sync::sync_folder::sha256_text("# 标题"))
        .await;

    let cfg = env.config(2);
    let files = get_files(&cfg, &mut db.conn).await.unwrap();
    create_output_files(&cfg, &files, &mut db.conn)
        .await
        .unwrap();

    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        1,
        "内容一样就不该重传"
    );
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        0,
        "内容一样还去删，等于把知识库里那份白删一遍"
    );
    let row = DBfunc::get_file(&mut db.conn, &src).await.unwrap().unwrap();
    assert_eq!(
        row.rag_id.as_deref(),
        Some("doc-1"),
        "沿用知识库里那一份的 doc_id"
    );
    assert_eq!(
        row.uploaded_sha256.as_deref(),
        Some(rag_sync::sync_folder::sha256_text("# 标题").as_str()),
        "库里要记着这份内容的指纹，下一轮才认得出它没变"
    );
}

/// 内容变了：旧文档必须让位，删了重传。
#[tokio::test]
async fn changed_content_still_deletes_and_reuploads() {
    let mut db = common::test_db().await;
    let (env, server) = conflict_env("# 第二版").await;
    let src = env.write_source("a.pdf", "内容真的变了");

    // 上一轮传的是别的内容
    db.insert_file_row_full(
        1,
        &src,
        "旧的源文件指纹",
        1,
        Some("doc-1"),
        Some("sync/a.pdf.md"),
    )
    .await;
    db.set_uploaded_sha256(&src, &rag_sync::sync_folder::sha256_text("# 第一版"))
        .await;

    let cfg = env.config(2);
    let files = get_files(&cfg, &mut db.conn).await.unwrap();
    create_output_files(&cfg, &files, &mut db.conn)
        .await
        .unwrap();

    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        2,
        "内容变了就得重传（第一次 409，删完再传）"
    );
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        1,
        "内容变了，旧文档必须让位"
    );
    let row = DBfunc::get_file(&mut db.conn, &src).await.unwrap().unwrap();
    assert_eq!(row.rag_id.as_deref(), Some("doc-2"), "要用新传的 doc_id");
}

/// 库里没记过内容指纹（加列之前入库的行）：没有依据，老老实实删了重传。
#[tokio::test]
async fn conflict_without_a_stored_hash_still_deletes_and_reuploads() {
    let mut db = common::test_db().await;
    let (env, server) = conflict_env("# 标题").await;
    let src = env.write_source("a.pdf", "内容");

    db.insert_file_row_full(
        1,
        &src,
        "旧的源文件指纹",
        1,
        Some("doc-1"),
        Some("sync/a.pdf.md"),
    )
    .await;

    let cfg = env.config(2);
    let files = get_files(&cfg, &mut db.conn).await.unwrap();
    create_output_files(&cfg, &files, &mut db.conn)
        .await
        .unwrap();

    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        1,
        "没有指纹就没有「内容没变」的依据，只能按老规矩删了重传"
    );
    assert_eq!(server.count_to("POST", "/documents/upload"), 2);
}

/// 冲突自愈的环境：上传一律 409，知识库里有一份叫 sync__a.pdf.md 的文档。
/// 路由按调用次序消费：第一次上传 409，删完之后文档列表就空了，第二次上传才成。
async fn conflict_env(markdown: &str) -> (Env, MockServer) {
    let mut env = Env::new();
    let mut routes = mineru_routes(markdown);
    routes.push(rag_login_ok());
    routes.push(
        Route::post("/documents/upload")
            .and(R::json(
                409,
                r#"{"detail":"File 'sync__a.pdf.md' already exists"}"#,
            ))
            .and(R::json(
                200,
                r#"{"status":"success","message":"ok","track_id":"tr-2"}"#,
            )),
    );
    routes.push(
        Route::post("/documents/paginated")
            .and(R::json(
                200,
                r#"{"documents":[{"id":"doc-1","status":"processed","file_path":"sync__a.pdf.md"}],
                    "pagination":{"page":1,"total_pages":1,"total_count":1,"has_next":false}}"#,
            ))
            // 删完之后文档就不在了
            .and(R::json(
                200,
                r#"{"documents":[],"pagination":{"page":1,"total_pages":1,"total_count":0,"has_next":false}}"#,
            )),
    );
    routes.push(Route::delete("/documents/delete_document").and(R::json(
        200,
        r#"{"status":"deletion_started","message":"ok","doc_id":"doc-1"}"#,
    )));
    routes.push(Route::get("/documents/track_status/tr-2").and(R::json(
        200,
        r#"{"track_id":"tr-2","total_count":1,"status_summary":{"processed":1},
            "documents":[{"id":"doc-2","status":"processed","file_path":"sync__a.pdf.md"}]}"#,
    )));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    (env, server)
}
