//! create_output_files 的端到端链路：扫描 -> MinerU 转换 -> 写产物 ->
//! 上传 LightRAG -> 落库。这一段改错了不会报错，只会几轮后表现为
//! 「文档没更新」或「原件不见了」。

mod common;

use common::{CannedResponse as R, Env, MockServer, Route};
use rag_sync::database::DBfunc;
use rag_sync::sync_folder::{
    RetryPolicy, create_output_files, create_output_files_with_retry, get_files,
};
use std::time::Duration;

/// 5 次尝试、不退避。失败用例看的是「失败冒泡 / 重试到成功」这些语义，
/// 而不是节奏 —— 用默认的 2s/4s/6s/8s 会让每个失败用例白等 20 秒。
/// 节奏本身由 `default_retry_policy_is_five_attempts_with_growing_backoff` 钉住。
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
    return vec![
        Route::post("/v1/uploads")
            .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#)),
        Route::put("/v1/uploads/up-1/content").and(R::json(200, "{}")),
        Route::post("/v1/uploads/up-1/complete")
            .and(R::json(200, r#"{"id":"up-1","status":"completed","file":{"id":"f-1"}}"#)),
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
    assert_eq!(
        env.output_text("a.pdf.md"),
        "# 标题\n\n正文",
        "产物内容应原样落盘"
    );
    assert!(out[0].starts_with(&env.output_dir));

    // 上传给 LightRAG 的就是刚写出来的产物文件，文件名以 .md 收尾：
    // LightRAG 按扩展名选解析器，产物里装的是 markdown，带 .docx 会判成
    // "File is not a valid DOCX document"。
    // 名字还带别名前缀：服务端以文件名为唯一键，目录前缀它看不见。
    let upload = server.requests_to("POST", "/documents/upload");
    assert_eq!(upload.len(), 1);
    assert!(
        upload[0].body_str().contains(r#"filename="sync__a.pdf.md""#),
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

    assert!(env.output_path("2024/一季度/a.pdf.md").exists());
}

/// 两个别名下的**同名**文件共用一个 output_dir：产物各落各的子目录，
/// 上传名也必须不同 —— 否则第二个会 409，自愈流程把第一个从知识库里删掉。
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
    assert!(uploaded.contains(&"sync__报告.pdf.md".to_string()), "{uploaded:?}");
    assert!(uploaded.contains(&"personal__报告.pdf.md".to_string()), "{uploaded:?}");
    // 上传名相同的话，第二个会 409，而 409 自愈要走去删文档的接口
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        0,
        "两个同名文件不该触发 409 自愈"
    );
}

/// 别名改了、目录也还在同一份 DB 里，但产物会落到另一个子目录 —— 这时必须
/// 重新处理：sha256 一样，光刷 run_id 会留下一份永远对不上的产物，
/// 知识库里也会一直是旧名字的文档。
#[tokio::test]
async fn changed_artifact_path_forces_reprocessing() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 标题").await;
    let src = env.write_source("a.pdf", "pdf 内容");
    let sha = rag_sync::sync_folder::sha256_file(&src).await.unwrap();

    // 上一轮跑完了，产物路径记的是别的别名
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

/// 产物路径在第一次落库时就要写进去。清理阶段全靠它定位产物，
/// 写晚了等于那一轮的产物失去了关联。
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

    // 产物内容与原文逐字一致，没有经过 MinerU
    assert_eq!(env.output_text("a.md"), md);
    assert_eq!(env.output_text("b.txt.md"), txt);
    assert_eq!(env.output_text("c.markdown.md"), markdown);
    // pdf 照旧走 MinerU，产物名不变
    assert_eq!(env.output_text("d.pdf.md"), "# MinerU 的产物");

    // 只有 pdf 碰过 MinerU；四个文件都上传了 LightRAG
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
    assert!(uploaded.contains(&"sync__b.txt.md".to_string()), "{uploaded:?}");
    assert!(uploaded.contains(&"sync__c.markdown.md".to_string()), "{uploaded:?}");

    // 透传的文件也照常落库
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
    let err = create_output_files_with_retry(&env.config(1), &files, &mut db.conn, no_backoff())
        .await
        .unwrap_err()
        .to_string();

    assert!(err.contains("a.pdf"), "{err}");
    assert!(err.contains("尝试 5 次"), "耗尽次数要把次数说清楚: {err}");
    // 5 次不多不少：少于 5 次是没重试够，多于 5 次是失控重试
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
    routes.push(
        Route::post("/documents/upload")
            .and(R::json(200, r#"{"status":"failure","message":"知识库不可写","track_id":""}"#)),
    );
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
        "每次重试都要重跑整条链路（转换 -> 写产物 -> 上传）"
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
        Route::post("/v1/parse/jobs").and(R::json(
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
    let res =
        create_output_files_with_retry(&env.config(1), &files, &mut db.conn, no_backoff()).await;

    assert!(res.is_err(), "单个文件失败必须冒泡，不能当成功收尾");
    assert_eq!(
        server.count_to("POST", "/v1/parse/jobs"),
        5,
        "冒泡之前要先把 5 次机会用完"
    );
}

/// 服务端抖两下就自愈：第 3 次成功后必须当成正常完成 —— 落盘、入库、
/// 记 run_id，一个都不许少。
#[tokio::test]
async fn transient_failure_is_retried_until_success() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let mut routes = mineru_routes("# 标题");
    // 队列按顺序消费：500 -> 500 -> 201，正好对上前两次失败、第三次成功
    routes[0] = Route::post("/v1/uploads")
        .and(R::json(500, r#"{"detail":"解析器抖动"}"#))
        .and(R::json(500, r#"{"detail":"解析器抖动"}"#))
        .and(R::json(201, r#"{"id":"up-1","status":"created","file":null}"#));
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
    assert_eq!(row.rag_id.as_deref(), Some("doc-1"), "成功那次的 doc_id 要落库");
}

/// 耗尽 5 次立刻中止整轮：同批后面排着的文件本轮不许开始，
/// 否则「中止」就只是把失败摊得更开。
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

/// 生产节奏钉在这里：5 次尝试、2s/4s/6s/8s 递增退避。
/// 这些是「要成立的行为」，改常量必须先改这个用例。
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
