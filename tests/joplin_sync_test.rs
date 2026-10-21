//! Joplin 同步流 -> LightRAG：笔记不落盘，取到直接进知识库。
//!
//! 盯住三件事：笔记的正文（不含末尾的元信息块）原样传进知识库、内容没变不重传、
//! 笔记被删了要能从知识库里撤掉。

mod common;

use common::{CannedResponse as R, MockServer, Route, TestDb};

use rag_sync::database::DBfunc;
use rag_sync::joplin_sync::sync_joplin;
use rag_sync::sync_config::{JoplinConfig, SyncConfig, SyncFolder};

const NOTE: &str = "aa11bb22cc33dd44ee55ff66aa77bb88.md";

/// 一条 delta 事件。
fn event(item_name: &str, type_: u8, updated_time: i64) -> String {
    format!(
        r#"{{"id":"ev-{updated_time}","item_id":"jop-item-1","item_name":"{item_name}",
             "previous_share_id":"","type":{type_},"updated_time":{updated_time},
             "jop_updated_time":{updated_time}}}"#
    )
}

/// 一页 delta 结果。
fn delta_page(items: &str, cursor: &str, has_more: bool) -> R {
    R::json(
        200,
        format!(r#"{{"items":[{items}],"cursor":"{cursor}","has_more":{has_more}}}"#),
    )
}

/// 笔记的原文：正文 + 末尾的元信息块。
fn note_content(body: &str, extra: &str) -> String {
    format!(
        "笔记标题\n\n{body}\n\nid: aa11bb22cc33dd44ee55ff66aa77bb88\n\
         parent_id: notebook-1\ncreated_time: 2024-01-31T00:00:00.000Z\n\
         updated_time: 2024-02-01T00:00:00.000Z\nencryption_applied: 0\n\
         type_: 1\n{extra}"
    )
}

/// LightRAG 侧的公共路由：登录、上传、轮询。
fn rag_routes(track_id: &str, doc_id: &str) -> Vec<Route> {
    vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"tok"}"#)),
        Route::post("/documents/upload").and(R::json(
            200,
            format!(r#"{{"status":"success","message":"ok","track_id":"{track_id}"}}"#),
        )),
        Route::get(&format!("/documents/track_status/{track_id}")).and(R::json(
            200,
            format!(
                r#"{{"track_id":"{track_id}","total_count":1,"status_summary":{{"processed":1}},
                     "documents":[{{"id":"{doc_id}","status":"processed","chunks_count":3,
                     "file_path":"joplin__{NOTE}"}}]}}"#
            ),
        )),
    ]
}

/// Joplin 侧的公共路由：登录。
fn joplin_login() -> Route {
    Route::post("/api/sessions").and(R::json(200, r#"{"id":"sess-1","user_id":"user-1"}"#))
}

/// 指向 mock 服务端的配置。
fn config(server: &MockServer, out_dir: &str) -> SyncConfig {
    SyncConfig {
        sync_folders: vec![SyncFolder {
            name: "sync".into(),
            path: "/tmp/不存在的同步目录".into(),
        }],
        output_dir: out_dir.into(),
        mineru_url: String::new(),
        lightrag_url: server.base_url.clone(),
        lightrag_username: "u".into(),
        lightrag_password: "p".into(),
        run_id: 1,
        joplin: Some(JoplinConfig {
            url: server.base_url.clone(),
            email: "a@b.c".into(),
            password: "p".into(),
            alias: "joplin".into(),
            limit: 100,
        }),
        mineru_concurrency: 1,
        lightrag_concurrency: 1,
    }
}

async fn setup(routes: Vec<Route>) -> (MockServer, TestDb, tempfile::TempDir) {
    let server = MockServer::start(routes).await;
    let db = common::test_db().await;
    let out = tempfile::tempdir().unwrap();
    (server, db, out)
}

/// 一次全量同步：笔记传进知识库，库里留下记录，本地一个文件都不落。
#[tokio::test]
async fn note_reaches_lightrag_without_touching_disk() {
    let mut routes = vec![
        joplin_login(),
        Route::get("/api/items/root:/:/delta?limit=100").and(delta_page(
            &event(NOTE, 1, 1_700_000_000_000),
            "c1",
            false,
        )),
        Route::get(&format!("/api/items/root:/{NOTE}:/content"))
            .and(R::text(200, note_content("笔记正文", ""))),
    ];
    routes.extend(rag_routes("t1", "doc-1"));
    let (server, mut db, out) = setup(routes).await;

    let summary = sync_joplin(&config(&server, out.path().to_str().unwrap()), &mut db.conn)
        .await
        .unwrap();
    assert_eq!(summary.uploaded, 1, "一篇笔记应该进库");
    assert_eq!(summary.unchanged, 0);
    assert_eq!(summary.deleted, 0);

    // 上传名带 joplin 前缀，正文只传正文，元信息块不带走
    let uploads = server.requests_to("POST", "/documents/upload");
    assert_eq!(uploads.len(), 1, "应该只上传一次");
    let body = uploads[0].body_str();
    assert!(
        body.contains(&format!("joplin__{NOTE}")),
        "上传名应为 joplin__<item_name>: {body}"
    );
    assert!(body.contains("笔记正文"), "正文要传过去: {body}");
    assert!(
        !body.contains("parent_id"),
        "元信息块不该传进知识库: {body}"
    );

    // 库里的记录
    let note = DBfunc::get_joplin_note(&mut db.conn, NOTE)
        .await
        .unwrap()
        .expect("笔记要有记录");
    assert_eq!(note.upload_name, format!("joplin__{NOTE}"));
    assert_eq!(note.rag_id.as_deref(), Some("doc-1"));
    assert_eq!(
        note.sha256,
        rag_sync::sync_folder::sha256_text("笔记标题\n\n笔记正文")
    );
    assert_eq!(note.item_id, "aa11bb22cc33dd44ee55ff66aa77bb88");

    // 游标记下来了，下轮从这里接着取
    assert_eq!(
        DBfunc::get_joplin_cursor(&mut db.conn)
            .await
            .unwrap()
            .as_deref(),
        Some("c1")
    );

    // 本地一个文件都不落
    let left: Vec<_> = std::fs::read_dir(out.path()).unwrap().collect();
    assert!(left.is_empty(), "笔记不该落盘: {left:?}");
}

/// 内容没变就不重传。
#[tokio::test]
async fn unchanged_note_is_not_uploaded_again() {
    let mut routes = vec![
        joplin_login(),
        Route::get("/api/items/root:/:/delta?limit=100").and(delta_page(
            &event(NOTE, 1, 1_700_000_000_000),
            "c1",
            false,
        )),
        Route::get("/api/items/root:/:/delta?limit=100&cursor=c1").and(delta_page(
            &event(NOTE, 2, 1_700_000_000_001),
            "c2",
            false,
        )),
        Route::get(&format!("/api/items/root:/{NOTE}:/content"))
            .and(R::text(200, note_content("笔记正文", ""))),
    ];
    routes.extend(rag_routes("t1", "doc-1"));
    let (server, mut db, out) = setup(routes).await;

    let cfg = config(&server, out.path().to_str().unwrap());
    let first = sync_joplin(&cfg, &mut db.conn).await.unwrap();
    assert_eq!(first.uploaded, 1);

    let second = sync_joplin(&cfg, &mut db.conn).await.unwrap();
    assert_eq!(second.uploaded, 0, "内容没变就不该重传");
    assert_eq!(second.unchanged, 1, "这一篇应该被认出来没变");
    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        1,
        "两轮加起来只该上传一次"
    );
    // 第二轮带着上轮的游标去取
    assert_eq!(
        server.count_to("GET", "/api/items/root:/:/delta?limit=100&cursor=c1"),
        1,
        "增量请求要带上服务端给的游标"
    );
}

/// 笔记被删了：知识库里的文档要撤掉，库里的记录也要删。
#[tokio::test]
async fn delete_event_removes_the_note_from_lightrag() {
    let mut routes = vec![
        joplin_login(),
        Route::get("/api/items/root:/:/delta?limit=100")
            .and(delta_page(&event(NOTE, 1, 1_700_000_000_000), "c1", false)),
        Route::get("/api/items/root:/:/delta?limit=100&cursor=c1")
            .and(delta_page(&event(NOTE, 3, 1_700_000_000_001), "c2", false)),
        Route::get(&format!("/api/items/root:/{NOTE}:/content"))
            .and(R::text(200, note_content("笔记正文", ""))),
        // 第一次查列表要能看到文档，删完再查就要看不到了
        Route::post("/documents/paginated")
            .and(R::json(
                200,
                format!(
                    r#"{{"documents":[{{"id":"doc-1","status":"processed","file_path":"joplin__{NOTE}"}}],
                        "pagination":{{"page":1,"total_pages":1,"total_count":1,"has_next":false}}}}"#
                ),
            ))
            .and(R::json(
                200,
                r#"{"documents":[],"pagination":{"page":1,"total_pages":1,"total_count":0,"has_next":false}}"#,
            )),
        Route::delete("/documents/delete_document").and(R::json(
            200,
            r#"{"status":"deletion_started","message":"","doc_id":"doc-1"}"#,
        )),
    ];
    routes.extend(rag_routes("t1", "doc-1"));
    let (server, mut db, out) = setup(routes).await;

    let cfg = config(&server, out.path().to_str().unwrap());
    sync_joplin(&cfg, &mut db.conn).await.unwrap();
    assert!(
        DBfunc::get_joplin_note(&mut db.conn, NOTE)
            .await
            .unwrap()
            .is_some()
    );

    let summary = sync_joplin(&cfg, &mut db.conn).await.unwrap();
    assert_eq!(summary.deleted, 1, "删掉的事件要撤掉知识库里的文档");
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        1,
        "应该发过一次删除"
    );
    assert!(
        DBfunc::get_joplin_note(&mut db.conn, NOTE)
            .await
            .unwrap()
            .is_none(),
        "笔记记录也要删掉"
    );
}

/// 笔记改了：旧文档要让位，新内容重传一遍。
#[tokio::test]
async fn changed_note_deletes_the_old_document_first() {
    let mut routes = vec![
        joplin_login(),
        Route::get("/api/items/root:/:/delta?limit=100")
            .and(delta_page(&event(NOTE, 1, 1_700_000_000_000), "c1", false)),
        Route::get("/api/items/root:/:/delta?limit=100&cursor=c1")
            .and(delta_page(&event(NOTE, 2, 1_700_000_000_001), "c2", false)),
        // 同一篇笔记，先旧内容后新内容
        Route::get(&format!("/api/items/root:/{NOTE}:/content"))
            .and(R::text(200, note_content("旧正文", "")))
            .and(R::text(200, note_content("新正文", ""))),
        // 第一次查列表要能看到旧文档，删完再查就要看不到了
        Route::post("/documents/paginated")
            .and(R::json(
                200,
                format!(
                    r#"{{"documents":[{{"id":"doc-1","status":"processed","file_path":"joplin__{NOTE}"}}],
                        "pagination":{{"page":1,"total_pages":1,"total_count":1,"has_next":false}}}}"#
                ),
            ))
            .and(R::json(
                200,
                r#"{"documents":[],"pagination":{"page":1,"total_pages":1,"total_count":0,"has_next":false}}"#,
            )),
        Route::delete("/documents/delete_document").and(R::json(
            200,
            r#"{"status":"deletion_started","message":"","doc_id":"doc-1"}"#,
        )),
    ];
    routes.extend(rag_routes("t1", "doc-1"));
    let (server, mut db, out) = setup(routes).await;

    let cfg = config(&server, out.path().to_str().unwrap());
    sync_joplin(&cfg, &mut db.conn).await.unwrap();

    let summary = sync_joplin(&cfg, &mut db.conn).await.unwrap();
    assert_eq!(summary.uploaded, 1, "改了的笔记要重传");
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        1,
        "旧文档要先让位，否则同名重传会被 409 拦下"
    );
    assert_eq!(server.count_to("POST", "/documents/upload"), 2);

    let note = DBfunc::get_joplin_note(&mut db.conn, NOTE)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        note.sha256,
        rag_sync::sync_folder::sha256_text("笔记标题\n\n新正文"),
        "记录里要是新内容的指纹"
    );
}

/// 客户端删掉的笔记是软删除：正文还在，但不该进知识库。
#[tokio::test]
async fn soft_deleted_note_is_not_uploaded() {
    let mut routes = vec![
        joplin_login(),
        Route::get("/api/items/root:/:/delta?limit=100").and(delta_page(
            &event(NOTE, 1, 1_700_000_000_000),
            "c1",
            false,
        )),
        Route::get(&format!("/api/items/root:/{NOTE}:/content")).and(R::text(
            200,
            note_content("笔记正文", "deleted_time: 1792551787908\n"),
        )),
    ];
    routes.extend(rag_routes("t1", "doc-1"));
    let (server, mut db, out) = setup(routes).await;

    let summary = sync_joplin(&config(&server, out.path().to_str().unwrap()), &mut db.conn)
        .await
        .unwrap();
    assert_eq!(summary.uploaded, 0, "删掉的笔记不进知识库");
    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        0,
        "不该发出上传请求"
    );
}

/// 变更记录、笔记本这些不是笔记，跳过。
#[tokio::test]
async fn non_note_items_are_skipped() {
    let mut routes = vec![
        joplin_login(),
        Route::get("/api/items/root:/:/delta?limit=100").and(delta_page(
            &event(NOTE, 1, 1_700_000_000_000),
            "c1",
            false,
        )),
        Route::get(&format!("/api/items/root:/{NOTE}:/content")).and(R::text(
            200,
            "id: aa11bb22cc33dd44ee55ff66aa77bb88\nparent_id: \nitem_type: 1\n\
             title_diff: \"[]\"\nbody_diff: \"[]\"\ntype_: 13\n",
        )),
    ];
    routes.extend(rag_routes("t1", "doc-1"));
    let (server, mut db, out) = setup(routes).await;

    let summary = sync_joplin(&config(&server, out.path().to_str().unwrap()), &mut db.conn)
        .await
        .unwrap();
    assert_eq!(summary.uploaded, 0);
    assert_eq!(summary.skipped, 1, "变更记录不是笔记");
    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        0,
        "不该发出上传请求"
    );
}

/// 游标失效时，服务端要求从头全量同步。
#[tokio::test]
async fn invalid_cursor_falls_back_to_full_sync() {
    let mut routes = vec![
        joplin_login(),
        // 带游标的请求被判失效
        Route::get("/api/items/root:/:/delta?limit=100&cursor=old").and(R::json(
            400,
            r#"{"error":"Delta cursor is invalid and the complete data should be resynced","code":"resyncRequired"}"#,
        )),
        Route::get("/api/items/root:/:/delta?limit=100")
            .and(delta_page(&event(NOTE, 1, 1_700_000_000_000), "c1", false)),
        Route::get(&format!("/api/items/root:/{NOTE}:/content"))
            .and(R::text(200, note_content("笔记正文", ""))),
    ];
    routes.extend(rag_routes("t1", "doc-1"));
    let (server, mut db, out) = setup(routes).await;

    // 先记一个失效的游标
    DBfunc::set_joplin_cursor(&mut db.conn, "old")
        .await
        .unwrap();

    let summary = sync_joplin(&config(&server, out.path().to_str().unwrap()), &mut db.conn)
        .await
        .unwrap();
    assert_eq!(summary.uploaded, 1, "从头同步要把笔记传进去");
    assert_eq!(
        server.count_to("GET", "/api/items/root:/:/delta?limit=100"),
        1,
        "应该不带游标重取一次"
    );
    assert_eq!(
        DBfunc::get_joplin_cursor(&mut db.conn)
            .await
            .unwrap()
            .as_deref(),
        Some("c1"),
        "新游标要记下来"
    );
}
