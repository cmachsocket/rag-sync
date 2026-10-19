//! 回归防护：每条对应一个曾经真实存在、且会**丢用户数据**的缺陷。
//! 断言已翻转为正确行为，作用是挡回归 —— 哪天又红了多半是真退化，别直接改断言。
//! 按缺陷编号排列，与修复记录一一对应。

mod common;

use common::{CannedResponse as R, Env, MockServer, Route};
use rag_sync::database::DBfunc;
use rag_sync::lightrag_requests::canonical_basename;
use rag_sync::sync_folder::{create_output_files, delete_old_files, get_files};

fn rag_routes(doc_id: &str) -> Vec<Route> {
    return vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload")
            .and(R::json(200, r#"{"status":"success","message":"ok","track_id":"tr-1"}"#)),
        Route::get("/documents/track_status/tr-1").and(R::json(
            200,
            format!(
                r#"{{"track_id":"tr-1","total_count":1,"status_summary":{{"processed":1}},
                     "documents":[{{"id":"{doc_id}","status":"processed","file_path":"a.md"}}]}}"#
            ),
        )),
    ]
}

fn mineru_routes(md: &str) -> Vec<Route> {
    return vec![
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
                serde_json::Value::String(md.to_string())
            ),
        )),
    ]
}

async fn env_with_services(md: &str) -> (Env, MockServer) {
    let mut env = Env::new();
    let mut routes = mineru_routes(md);
    routes.extend(rag_routes("doc-1"));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    (env, server)
}

/// 跑完一轮「扫描 + 转换」，不碰删除步骤
async fn sync_once(
    env: &Env,
    db: &mut common::TestDb,
    run_id: i32,
) -> Vec<rag_sync::db_model::File> {
    let files = get_files(&env.config(run_id), &mut db.conn).await.unwrap();
    create_output_files(&env.config(run_id), &files, &mut db.conn)
        .await
        .unwrap();
    files
}

// ================================================================ 1
// insert_file 曾把 run_id 写进 id 主键列

#[tokio::test]
async fn insert_writes_run_id_and_autoincrements_id() {
    let mut db = common::test_db().await;

    DBfunc::insert_file(&mut db.conn, "/sync/a.pdf", "sha-a", 7, Some("doc-1"))
        .await
        .unwrap();

    let row = DBfunc::get_file(&mut db.conn, "/sync/a.pdf").await.unwrap().unwrap();
    assert_eq!(row.run_id, 7, "轮次号必须落在 run_id 列");
    assert_eq!(row.id, 1, "id 交给 AUTOINCREMENT，不该由调用方指定");
}

#[tokio::test]
async fn multiple_files_insert_in_same_run() {
    let mut db = common::test_db().await;
    // 曾经两个文件都用 run_id 当 id，第二个直接 UNIQUE 主键冲突
    for (path, sha) in [("/sync/a.pdf", "s1"), ("/sync/b.pdf", "s2"), ("/sync/c.pdf", "s3")] {
        DBfunc::insert_file(&mut db.conn, path, sha, 9, Some("doc")).await.unwrap();
    }
    let ids = db.row_ids().await;
    assert_eq!(ids, vec![1, 2, 3], "三个文件都要有各自的主键");
    assert!(db.rows().await.iter().all(|f| f.run_id == 9));
}

#[tokio::test]
async fn delete_file_touches_only_target_row() {
    let mut db = common::test_db().await;
    db.insert_file_row(1, "/sync/a.pdf", "s1", 1, None).await;
    db.insert_file_row(2, "/sync/b.pdf", "s2", 1, None).await;

    DBfunc::delete_file(&mut db.conn, "/sync/a.pdf").await.unwrap();

    assert!(!DBfunc::file_exists(&mut db.conn, "/sync/a.pdf").await.unwrap());
    assert!(DBfunc::file_exists(&mut db.conn, "/sync/b.pdf").await.unwrap());
    // 删不存在的行不算失败
    DBfunc::delete_file(&mut db.conn, "/sync/没有.pdf").await.unwrap();
}

// ================================================================ 2
// run_id 停在 0 -> 同一轮就把刚处理完的原件当成旧文件删掉

#[tokio::test]
async fn source_documents_survive_a_run() {
    let mut db = common::test_db().await;
    let (env, _s) = env_with_services("# 标题").await;
    let src = env.write_source("report.pdf", "用户的原始文档");

    sync_once(&env, &mut db, 1).await;
    assert_eq!(db.run_id_of(&src).await, 1, "本轮处理过的必须标成当前轮次");

    delete_old_files(&env.config(1), &mut db.conn).await.unwrap();

    assert!(
        std::path::Path::new(&src).exists(),
        "本轮的文件不该被当成本轮之前的旧文件"
    );
}

// ================================================================ 3
// 跳过判断拿 DB 的 sha 比 DB 的 sha -> 内容改了也看不见

#[tokio::test]
async fn changed_content_is_reprocessed() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 第一版").await;
    let src = env.write_source("a.pdf", "第一次的内容");

    sync_once(&env, &mut db, 1).await;
    assert_eq!(env.output_text("a.pdf.md"), "# 第一版");

    // 用户改了文件
    std::fs::write(&src, "完全不同的内容").unwrap();
    sync_once(&env, &mut db, 2).await;

    assert_eq!(
        server.count_to("POST", "/v1/file_parse"),
        2,
        "内容变了必须重跑 MinerU"
    );
    assert_eq!(server.count_to("POST", "/documents/upload"), 2, "产物变了要重传");
    assert_eq!(
        DBfunc::get_sha256(&mut db.conn, &src).await.unwrap(),
        Some(rag_sync::sync_folder::sha256_file(&src).await.unwrap()),
        "库里要记下新的 sha256，否则下一轮又会重跑"
    );
    assert_eq!(db.run_id_of(&src).await, 2);
}

#[tokio::test]
async fn unchanged_content_only_refreshes_run_id() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 标题").await;
    let src = env.write_source("a.pdf", "内容不变");

    sync_once(&env, &mut db, 1).await;
    sync_once(&env, &mut db, 2).await;

    assert_eq!(server.count_to("POST", "/v1/file_parse"), 1, "没变就不该重跑");
    assert_eq!(
        db.run_id_of(&src).await,
        2,
        "跳过时也必须刷新 run_id，否则下一轮会被当成用户已删除"
    );
}

// ================================================================ 4
// delete_old_files 曾删同步目录里的用户原件

#[tokio::test]
async fn cleanup_only_removes_generated_artifacts() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let mut routes = rag_routes("doc-old");
    // 删除确认：文档列表里已经没有它了
    routes.push(
        Route::post("/documents/paginated")
            .and(R::json(
                200,
                r#"{"documents":[{"id":"doc-old","status":"processed","file_path":"gone.md"}],
                     "pagination":{"page":1,"total_pages":1,"total_count":1,"has_next":false}}"#,
            ))
            .and(R::json(
                200,
                r#"{"documents":[],"pagination":{"page":1,"total_pages":1,"total_count":0,"has_next":false}}"#,
            )),
    );
    routes.push(Route::delete("/documents/delete_document").and(R::json(
        200,
        r#"{"status":"deletion_started","message":"ok","doc_id":"doc-old"}"#,
    )));
    let server = MockServer::start(routes).await;
    env.lightrag_url = server.base_url.clone();

    let src = env.write_source("gone.md", "用户的原始文档");
    let out = env.output_path("gone.md");
    std::fs::create_dir_all(env.output_dir()).unwrap();
    std::fs::write(&out, "# 过期产物").unwrap();
    db.insert_file_row(1, &src, "sha", 1, Some("doc-old")).await;

    // 本轮 run_id=2，没扫到它 -> 属于上一轮遗留
    delete_old_files(&env.config(2), &mut db.conn).await.unwrap();

    assert!(
        std::path::Path::new(&src).exists(),
        "同步目录里的原始文档属于用户，任何情况下都不能删"
    );
    assert!(!out.exists(), "过期产物要清掉");
    assert_eq!(
        server.requests_to("DELETE", "/documents/delete_document")[0].json()["doc_ids"][0],
        "doc-old",
        "知识库里的文档也要撤掉，否则库里会一直留着旧内容"
    );
    assert!(
        !DBfunc::file_exists(&mut db.conn, &src).await.unwrap(),
        "DB 行要一并删掉，否则下一轮还会被当成待清理项"
    );
}

#[tokio::test]
async fn cleanup_skips_service_when_nothing_is_old() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 标题").await;
    env.write_source("a.pdf", "内容");
    sync_once(&env, &mut db, 1).await;

    let before = server.count_to("POST", "/login");
    // 同一轮收尾：本轮扫到的文件 run_id 就是 1，没有「旧文件」
    delete_old_files(&env.config(1), &mut db.conn).await.unwrap();

    assert_eq!(
        server.count_to("POST", "/login"),
        before,
        "没东西要清就别登录 —— 否则 LightRAG 挂掉会连一轮空同步都跑不完"
    );
    assert!(env.output_path("a.pdf.md").exists(), "本轮文件的产物要留着");
    assert!(
        DBfunc::file_exists(&mut db.conn, &format!("{}/a.pdf", env.sync_dir))
            .await
            .unwrap(),
        "本轮文件的记录也要留着"
    );
}

#[tokio::test]
async fn current_run_files_are_never_cleaned() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 标题").await;
    env.write_source("a.pdf", "内容");
    sync_once(&env, &mut db, 5).await;

    delete_old_files(&env.config(5), &mut db.conn).await.unwrap();

    assert!(env.output_path("a.pdf.md").exists());
    assert_eq!(server.count_to("DELETE", "/documents/delete_document"), 0);
}

#[tokio::test]
async fn no_pending_files_means_no_service_calls() {
    let mut db = common::test_db().await;
    let (env, server) = env_with_services("# 标题").await;
    sync_once(&env, &mut db, 1).await;
    let before = server.count_to("POST", "/login");

    sync_once(&env, &mut db, 2).await;

    assert_eq!(
        server.count_to("POST", "/login"),
        before,
        "这轮没事可做，服务不可达也不该让它失败"
    );
}

/// 完整生命周期：两轮同步，中间用户自己删掉一个文件。
/// 断言「用户删掉的」和「用户还在的」两种结果都对。
#[tokio::test]
async fn two_rounds_cleanup_deleted_keep_rest() {
    let mut db = common::test_db().await;
    let mut env = Env::new();
    let mut routes = mineru_routes("# 标题");
    routes.extend(rag_routes("doc-keep"));
    routes.push(Route::post("/documents/upload").and(R::json(
        200,
        r#"{"status":"success","message":"ok","track_id":"tr-1"}"#,
    )));
    routes.push(Route::get("/documents/track_status/tr-1").and(R::json(
        200,
        r#"{"track_id":"tr-1","total_count":1,"status_summary":{"processed":1},
            "documents":[{"id":"doc-gone","status":"processed","file_path":"gone.md"}]}"#,
    )));
    routes.push(Route::delete("/documents/delete_document").and(R::json(
        200,
        r#"{"status":"deletion_started","message":"ok","doc_id":"doc-gone"}"#,
    )));
    routes.push(Route::post("/documents/paginated").and(R::json(
        200,
        r#"{"documents":[],"pagination":{"page":1,"total_pages":1,"total_count":0,"has_next":false}}"#,
    )));
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();

    let keep = env.write_source("keep.md", "留着");
    let gone = env.write_source("gone.md", "用户后来删掉了");

    // 第一轮：两个文件都入库
    sync_once(&env, &mut db, 1).await;
    assert!(DBfunc::file_exists(&mut db.conn, &keep).await.unwrap());
    assert!(DBfunc::file_exists(&mut db.conn, &gone).await.unwrap());
    assert_eq!(
        db.run_id_of(&gone).await,
        1,
        "新建的文件也要记下轮次，否则下一轮会被当成上轮遗留"
    );

    // 用户自己删掉了 gone.md
    std::fs::remove_file(&gone).unwrap();

    // 第二轮
    let files = get_files(&env.config(2), &mut db.conn).await.unwrap();
    assert_eq!(files.len(), 1, "只扫到剩下的那个");
    create_output_files(&env.config(2), &files, &mut db.conn)
        .await
        .unwrap();
    delete_old_files(&env.config(2), &mut db.conn).await.unwrap();

    assert!(
        std::path::Path::new(&keep).exists(),
        "用户还留着的文件不能被碰"
    );
    assert!(env.output_path("keep.md").exists(), "它的产物要留着");
    assert!(
        !env.output_path("gone.md").exists(),
        "用户删掉的文件，产物要跟着清掉"
    );
    assert!(
        !DBfunc::file_exists(&mut db.conn, &gone).await.unwrap(),
        "对应的 DB 行也要清掉"
    );
    assert!(DBfunc::file_exists(&mut db.conn, &keep).await.unwrap());
    assert_eq!(db.run_id_of(&keep).await, 2, "还在的文件轮次要跟上");
}

// ================================================================ 5
// ensure_file_id 曾把 DB 行号当成 MinerU 的 file_id

#[tokio::test]
async fn reprocess_uses_real_mineru_file_id() {
    let d = tempfile::tempdir().unwrap();
    let server = MockServer::start(mineru_routes("# 转换结果")).await;
    let path = d.path().join("a.pdf");
    std::fs::write(&path, "x").unwrap();

    // id=42 是 DB 行号；库里没有 MinerU 的 file_id
    let file = rag_sync::db_model::File {
        id: 42,
        path: path.to_string_lossy().into_owned(),
        sha256: "abc".into(),
        rag_id: Some("doc-x".into()),
        run_id: 2,
    };
    let cfg = rag_sync::sync_config::SyncConfig {
        sync_dir: d.path().to_string_lossy().into_owned(),
        output_dir: d.path().to_string_lossy().into_owned(),
        mineru_url: server.base_url.clone(),
        lightrag_url: String::new(),
        lightrag_username: "u".into(),
        lightrag_password: "p".into(),
        run_id: 2,
    };

    rag_sync::mineru_requests::mineru_tackle(&MockServer::client(), &file, &cfg)
        .await
        .unwrap();

    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        1,
        "没有可复用的 MinerU file_id，就必须走上传"
    );
    let body = server.requests_to("POST", "/v1/file_parse")[0].json();
    assert_eq!(
        body["files"][0]["source"]["file_id"], "f-1",
        "解析要用上传返回的 file_id，不是 DB 行号"
    );
}

// ================================================================ 6
// canonical_basename 剥 hint 后残留点号

#[test]
fn parser_hint_normalises_to_same_key() {
    for hint in ["[native]", "[legacy]"] {
        assert_eq!(
            canonical_basename(&format!("a.{hint}.md")),
            canonical_basename("a.md"),
            "{hint} 没被完全剥掉"
        );
        assert_eq!(canonical_basename(&format!("a{hint}.md")), "a");
    }
    assert_eq!(canonical_basename("report.[native].docx"), "report");
    assert_eq!(canonical_basename("a.[native].[legacy].md"), "a", "叠着的也要剥干净");
    // 大小写与扩展名差异不影响归一
    assert_eq!(canonical_basename("A.MD"), canonical_basename("a.md"));
    // 没见过的 hint 不能乱剥
    assert_eq!(canonical_basename("a.[weird].md"), "a.[weird]");
}

#[tokio::test]
async fn conflict_selfheal_works_with_parser_hint() {
    let client = MockServer::client();
    let server = MockServer::start(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload")
            .and(R::json(409, r#"{"detail":"Document 'a.[native].md' already exists"}"#))
            .and(R::json(200, r#"{"status":"success","message":"ok","track_id":"tr-2"}"#)),
        Route::post("/documents/paginated")
            .and(R::json(
                200,
                r#"{"documents":[{"id":"doc-h","status":"processed","file_path":"a.[native].md"}],
                     "pagination":{"page":1,"total_pages":1,"total_count":1,"has_next":false}}"#,
            ))
            .and(R::json(
                200,
                r#"{"documents":[],"pagination":{"page":1,"total_pages":1,"total_count":0,"has_next":false}}"#,
            )),
        Route::delete("/documents/delete_document").and(R::json(
            200,
            r#"{"status":"deletion_started","message":"ok","doc_id":"doc-h"}"#,
        )),
        Route::get("/documents/track_status/tr-2").and(R::json(
            200,
            r#"{"track_id":"tr-2","total_count":1,"status_summary":{"processed":1},
                "documents":[{"id":"doc-new","status":"processed","file_path":"a.md"}]}"#,
        )),
    ])
    .await;

    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("a.md");
    std::fs::write(&p, "x").unwrap();

    let rag = rag_sync::lightrag_requests::RAGRequest::new(&client, &server.base_url, "u", "p")
        .await
        .unwrap();
    let doc_id = rag.upload_file(&p.to_string_lossy()).await.unwrap();

    assert_eq!(doc_id, "doc-new", "反查到带 hint 的旧文档、删掉、再重传");
    assert_eq!(
        server.requests_to("DELETE", "/documents/delete_document")[0].json()["doc_ids"][0],
        "doc-h"
    );
}

// ================================================================ 7
// 配置里的 ~ 不展开

#[tokio::test]
async fn tilde_in_config_is_expanded() {
    let mut db = common::test_db().await;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("config.yml");
    std::fs::write(
        &p,
        "sync_dir: \"~/Documents\"\noutput_dir: \"~\"\nmineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n",
    )
    .unwrap();

    let c = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, p.to_str().unwrap())
        .await
        .unwrap();

    let home = std::env::var("HOME").expect("本测试环境应有 HOME");
    assert_eq!(
        c.sync_dir,
        std::path::Path::new(&home).join("Documents").to_string_lossy(),
        "~ 会被当成普通字符，工作目录下根本没有这个相对路径"
    );
    assert_eq!(c.output_dir, home);
}

#[tokio::test]
async fn tilde_inside_path_is_left_alone() {
    let mut db = common::test_db().await;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("config.yml");
    std::fs::write(
        &p,
        "sync_dir: \"/data/~backup\"\noutput_dir: \"/data/a~b\"\n\
         mineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n",
    )
    .unwrap();

    let c = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, p.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(c.sync_dir, "/data/~backup");
    assert_eq!(c.output_dir, "/data/a~b");
}

// ================================================================ 8
// 同步目录不存在时静默跑完一轮空同步

#[tokio::test]
async fn missing_sync_dir_is_an_error() {
    let mut db = common::test_db().await;
    let env = Env::new();
    let mut cfg = env.config(1);
    cfg.sync_dir = "/no/such/dir".into();

    let err = get_files(&cfg, &mut db.conn).await.unwrap_err().to_string();
    assert!(
        err.contains("/no/such/dir"),
        "路径写错必须让整轮停下来，不能与「目录里没有文件」混为一谈: {err}"
    );
}
