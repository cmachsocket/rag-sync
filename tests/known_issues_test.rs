#![allow(non_snake_case)]
//! **已知缺陷的行为快照。**
//!
//! 这里的每一条断言的都是「当前实际行为」，而这个行为是错的。每条都写清了
//! 「应该是什么」。修掉某个缺陷时，把对应的断言翻过来即可 —— 翻过来之后测试
//! 才会拦住回归。
//!
//! 放在这里而不是散在各测试文件里，是为了让人一眼看出：`cargo test` 全绿
//! 不等于同步流程是对的。

mod common;

use common::{CannedResponse as R, Env, MockServer, Route};
use rag_sync::database::DBfunc;
use rag_sync::lightrag_requests::canonical_basename;
use rag_sync::sync_folder::{create_output_files, delete_old_files, get_files};

fn rag_路由() -> Vec<Route> {
    vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload")
            .and(R::json(200, r#"{"status":"success","message":"ok","track_id":"tr-1"}"#)),
        Route::get("/documents/track_status/tr-1").and(R::json(
            200,
            r#"{"track_id":"tr-1","total_count":1,"status_summary":{"processed":1},
                "documents":[{"id":"doc-1","status":"processed","file_path":"a.pdf"}]}"#,
        )),
    ]
}

fn mineru_路由(md: &str) -> Vec<Route> {
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
                serde_json::Value::String(md.to_string())
            ),
        )),
    ]
}

async fn 环境(md: &str) -> (Env, MockServer) {
    let mut env = Env::new();
    let mut routes = mineru_路由(md);
    routes.extend(rag_路由());
    let server = MockServer::start(routes).await;
    env.mineru_url = server.base_url.clone();
    env.lightrag_url = server.base_url.clone();
    (env, server)
}

// ================================================================ 缺陷 1
// insert_file 的第三个形参是 `id`，调用方传的却是 `config.run_id`。
// 结果：id 列被写成轮次号，run_id 列停在 schema 默认值 0。

#[tokio::test]
async fn 缺陷1_插入时轮次号写进了_主键列() {
    let mut db = common::test_db().await;

    // create_output_files 里的原句：insert_file(conn, path, sha, config.run_id, Some(&result))
    DBfunc::insert_file(&mut db.conn, "/sync/a.pdf", "sha-a", 7, Some("doc-1"))
        .await
        .unwrap();

    let row = DBfunc::get_file(&mut db.conn, "/sync/a.pdf").await.unwrap().unwrap();
    assert_eq!(row.id, 7, "第三个数落在了 id 列上");
    assert_eq!(
        row.run_id, 0,
        "run_id 应写 7，实际留在 schema 默认值 0"
    );
    // 修好之后：第三个数应当是 run_id，id 交给 AUTOINCREMENT
}

#[tokio::test]
async fn 缺陷2_同一轮插入第二个文件_主键冲突() {
    let mut db = common::test_db().await;
    // 两个文件都用 run_id=7 当第三个数 -> id 列重复
    DBfunc::insert_file(&mut db.conn, "/sync/a.pdf", "sha-a", 7, Some("doc-1"))
        .await
        .unwrap();
    let err = DBfunc::insert_file(&mut db.conn, "/sync/b.pdf", "sha-b", 7, Some("doc-2"))
        .await
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("UNIQUE") || err.contains("constraint"),
        "预期主键冲突，实际: {err}"
    );
    // 修好之后：第二个文件应当正常入库
}

// ================================================================ 缺陷 2
// 刚入库的文件 run_id=0，delete_old_files 查「run_id < 本轮」的记录，
// 于是同一轮刚处理完的用户原件就被删掉。

#[tokio::test]
async fn 缺陷3_单轮同步后_用户的原始文档就没了() {
    let mut db = common::test_db().await;
    let (env, _s) = 环境("# 标题").await;
    let src = env.write_source("report.pdf", "用户的原始文档");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    create_output_files(&env.config(1), &files, &mut db.conn)
        .await
        .unwrap();
    assert!(std::path::Path::new(&src).exists(), "转换阶段不该删原件");

    delete_old_files(&env.config(1), &mut db.conn).await.unwrap();

    assert!(
        !std::path::Path::new(&src).exists(),
        "【缺陷】刚处理完的用户原件被当成了上一轮的旧文件删掉。\
         修好之后应改为：原件一个都不能动，本轮新增的行 run_id 等于本轮。"
    );
}

// ================================================================ 缺陷 3
// 跳过判断拿「DB 里的 sha」和「DB 里的 sha」比，等于恒真。

#[tokio::test]
async fn 缺陷4_文件内容改了也不会重新转换() {
    let mut db = common::test_db().await;
    let (env, server) = 环境("# 新内容").await;
    let src = env.write_source("a.pdf", "第一次的内容");
    let old_sha = rag_sync::sync_folder::sha256_file(&src).await.unwrap();
    db.insert_file_row(1, &src, &old_sha, 1, Some("doc-old")).await;

    // 用户改了文件
    std::fs::write(&src, "完全不同的内容").unwrap();

    let files = get_files(&env.config(2), &mut db.conn).await.unwrap();
    create_output_files(&env.config(2), &files, &mut db.conn)
        .await
        .unwrap();

    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        0,
        "【缺陷】get_files 对库中已有的文件不重算 sha256，\
         create_output_files 又拿 DB 值和 DB 值比，\
         于是内容改了也永远看不见。修好之后这里应是 1，且产物与 rag_id 都要刷新。"
    );
}

#[tokio::test]
async fn 缺陷5_跳过的文件_run_id_不刷新_下一轮被当成旧文件() {
    let mut db = common::test_db().await;
    let (env, _s) = 环境("# 标题").await;
    let src = env.write_source("a.pdf", "内容没变");
    let sha = rag_sync::sync_folder::sha256_file(&src).await.unwrap();
    db.insert_file_row(1, &src, &sha, 1, Some("doc-old")).await;

    // 第二轮：文件没变 -> 被跳过 -> run_id 留在 1
    let files = get_files(&env.config(2), &mut db.conn).await.unwrap();
    create_output_files(&env.config(2), &files, &mut db.conn)
        .await
        .unwrap();
    assert_eq!(
        db.run_id_of(&src).await,
        1,
        "跳过时没有 update_file_run_id，「本轮见过」这个信号丢了"
    );

    delete_old_files(&env.config(2), &mut db.conn).await.unwrap();
    assert!(
        !std::path::Path::new(&src).exists(),
        "【缺陷】本轮明明扫到过这个文件，却因为跳过时没刷新 run_id 而被删掉。\
         修好之后：跳过分支也要把 run_id 刷成本轮。"
    );
}

// ================================================================ 缺陷 4
// delete_old_files 删的是 files.path，也就是同步目录里的用户原件。

#[tokio::test]
async fn 缺陷6_删除逻辑删的是原件_不是输出产物() {
    let mut db = common::test_db().await;
    let env = Env::new();
    let src = env.write_source("report.pdf", "原件");
    let out = env.output_path("report.pdf");
    std::fs::create_dir_all(env.output_dir()).unwrap();
    std::fs::write(&out, "# 上一轮的 markdown").unwrap();
    db.insert_file_row(1, &src, "sha", 2, Some("doc-1")).await;

    delete_old_files(&env.config(3), &mut db.conn).await.unwrap();

    assert!(
        !std::path::Path::new(&src).exists(),
        "【缺陷】files.path 指向同步目录里的用户原件，delete_old_files 删的正是它。\
         修好之后原件应当保留。"
    );
    assert!(
        out.exists(),
        "【缺陷】该被清理的是 output_dir 里这份过期产物；\
         RAG 里那条 doc-1 也没人删。修好之后：按 run_id 找旧记录 -> 删 output 产物 -> \
         delete_document -> 删 DB 行。"
    );
}

// ================================================================ 缺陷 5
// ensure_file_id 把 DB 行号当成 MinerU 的 file_id 传出去。

#[tokio::test]
async fn 缺陷7_把数据库行号当成了_矿U_file_id() {
    let d = tempfile::tempdir().unwrap();
    let mut routes = vec![
        Route::post("/v1/file_parse").and(R::json(
            200,
            r#"{"status":"completed","task_id":"t1","files":[
                 {"name":"a.pdf","status":"done","content":{"markdown":"ok"}}]}"#,
        )),
    ];
    // 上传路由一次都不该被用到
    routes.push(Route::post("/v1/uploads").once());
    let server = MockServer::start(routes).await;

    let path = d.path().join("a.pdf");
    std::fs::write(&path, "x").unwrap();
    let file = rag_sync::db_model::File {
        id: 42, // DB 行号
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

    rag_sync::mineru_requests::mineru_tackle(&reqwest::Client::new(), &file, &cfg)
        .await
        .unwrap();

    let body = server.requests_to("POST", "/v1/file_parse")[0].json();
    assert_eq!(
        body["files"][0]["source"]["file_id"], "42",
        "解析请求里带的是 DB 行号，不是 MinerU 的 file_id"
    );
    assert_eq!(
        server.count_to("POST", "/v1/uploads"),
        0,
        "库里明明没存 MinerU file_id，却跳过了上传流程"
    );
    // 修好之后：要么把 MinerU file_id 存进 DB 复用，要么每次都重新上传
}

// ================================================================ 缺陷 6
// canonical_basename 剥掉 "[native]" 后把分隔的点号留下了，
// 于是「a.md」和「a.[native].md」算出来是两个名字。

#[test]
fn 缺陷8_剥掉_parser_hint_后残留点号() {
    assert_eq!(canonical_basename("a.md"), "a");
    assert_eq!(
        canonical_basename("a.[native].md"),
        "a.",
        "剥 '[native]' 时连同一个分隔符一起剥掉才对"
    );
    assert_ne!(
        canonical_basename("a.md"),
        canonical_basename("a.[native].md"),
        "注释里写的是「两者指向同一条记录」，实现没做到"
    );
    // 修好之后：canonical_basename("a.[native].md") 应等于 canonical_basename("a.md")
}

#[tokio::test]
async fn 缺陷9_因此_409自愈对带hint的文档无效() {
    let client = reqwest::Client::new();
    let server = MockServer::start(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload").and(R::json(
            409,
            r#"{"detail":"Document 'a.[native].md' already exists"}"#,
        )),
        Route::post("/documents/paginated").and(R::json(
            200,
            r#"{"documents":[{"id":"doc-h","status":"processed","file_path":"a.[native].md"}],
                "pagination":{"page":1,"total_pages":1,"total_count":1,"has_next":false}}"#,
        )),
        Route::delete("/documents/delete_document").once(),
    ])
    .await;

    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("a.md");
    std::fs::write(&p, "x").unwrap();

    let rag = rag_sync::lightrag_requests::RAGRequest::new(&client, &server.base_url, "u", "p")
        .await
        .unwrap();
    let err = rag
        .upload_file(&p.to_string_lossy())
        .await
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("input 目录") || err.contains("找不到"),
        "预期：明明列表里有这条文档却没匹配上。实际错误: {err}"
    );
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        0,
        "没匹配上 -> 走人工确认分支，不删"
    );
    // 修好之后：应能反查到 doc-h、删掉、再重传成功
}

// ================================================================ 缺陷 7
// 配置里的 ~ 不展开。

#[tokio::test]
async fn 缺陷10_配置里的波浪号不被展开() {
    let mut db = common::test_db().await;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("config.yml");
    std::fs::write(
        &p,
        "sync_dir: \"~/Documents\"\noutput_dir: \"~/MinerU\"\n",
    )
    .unwrap();

    let c = rag_sync::sync_config::SyncConfig::from_file(
        &mut db.conn,
        p.to_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        c.sync_dir, "~/Documents",
        "~ 被当成普通字符，会在工作目录下找 ~/Documents 这个相对路径"
    );
    // 修好之后：应展开成 $HOME/Documents
}

// ================================================================ 缺陷 8
// 同步目录不存在 / 不可读时，WalkDir 的错误被 eprintln 吞掉，返回空 vec。

#[tokio::test]
async fn 缺陷11_目录不存在时静默跑完一轮空同步() {
    let mut db = common::test_db().await;
    let env = Env::new();
    let mut cfg = env.config(1);
    cfg.sync_dir = "/no/such/dir".into();

    let files = get_files(&cfg, &mut db.conn).await.unwrap();
    assert!(
        files.is_empty(),
        "目录路径写错时和「目录里没文件」长得一模一样，main 会照常推进 run_counter"
    );
    assert!(
        DBfunc::get_run_counter(&mut db.conn)
            .await
            .unwrap()
            .is_some(),
        "计数器照旧推进，下一轮的 run_id 直接跳过一轮"
    );
    // 修好之后：根目录不存在时应返回 Err，让 main 停下来
}
