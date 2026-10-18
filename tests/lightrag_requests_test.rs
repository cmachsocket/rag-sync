#![allow(non_snake_case)]
//! RAGRequest 对着 mock 服务端跑完整链路。
//!
//! 覆盖的是最贵的那条路径：409 同名冲突 -> 按文件名反查 doc_id -> 删除 ->
//! 重传 -> 轮询到终态。任何一步退化成「看起来成功」，知识库里就会留下
//! 旧文档或半截文档，而接口全程返回 200。

mod common;

use common::{CannedResponse as R, MockServer, Route};

use rag_sync::lightrag_requests::{basename, canonical_basename, RAGRequest};

fn 上传成功体(track: &str) -> R {
    R::json(
        200,
        format!(
            r#"{{"status":"success","message":"File 'a.md' uploaded successfully.","track_id":"{track}"}}"#
        ),
    )
}

fn track_已处理(doc_id: &str) -> R {
    R::json(
        200,
        format!(
            r#"{{"track_id":"t1","total_count":1,"status_summary":{{"processed":1}},
                 "documents":[{{"id":"{doc_id}","status":"processed","chunks_count":6,
                 "file_path":"a.md"}}]}}"#
        ),
    )
}

fn 文档页(docs: &str, has_next: bool) -> R {
    R::json(
        200,
        format!(
            r#"{{"documents":[{docs}],"pagination":{{"page":1,"total_pages":1,
                 "total_count":1,"has_next":{has_next}}}}}"#
        ),
    )
}

async fn 起服务(routes: Vec<Route>) -> (MockServer, tempfile::TempDir) {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("a.md"), "# a\n内容\n").unwrap();
    (MockServer::start(routes).await, d)
}

fn 上传路径(d: &tempfile::TempDir) -> String {
    d.path().join("a.md").to_string_lossy().into_owned()
}

async fn 客户端<'a>(
    server: &'a MockServer,
    client: &'a reqwest::Client,
) -> RAGRequest<'a> {
    RAGRequest::new(client, &server.base_url, "u", "p")
        .await
        .unwrap()
}

// ---------------------------------------------------------------- 文件名归一

#[test]
fn basename_只取最后一段() {
    assert_eq!(basename("/home/u/文档/a.md"), "a.md");
    assert_eq!(basename("a.md"), "a.md");
    assert_eq!(basename(""), "");
    // 没有文件名可取时退回原串，而不是变空串
    assert_eq!(basename("/"), "/");
}

#[test]
fn canonical_basename_不猜没见过的hint() {
    // 宁可匹配不上、报出来让人看一眼，也不要按错的名字删掉别人的文档
    assert_eq!(canonical_basename("abc.[weird].docx"), "abc.[weird]");
    assert_eq!(canonical_basename("abc.v2.docx"), "abc.v2");
}

#[test]
fn canonical_basename_只剥结尾的hint() {
    assert_eq!(canonical_basename("[native].abc.docx"), "[native].abc");
    assert_eq!(canonical_basename("abc.md"), "abc");
    assert_eq!(canonical_basename("abc"), "abc");
    // 多点文件名取最后一个点之前的部分
    assert_eq!(canonical_basename("a.b.c.docx"), "a.b.c");
    // 大小写统一，便于和服务端存的文件名对齐
    assert_eq!(canonical_basename("ABC.MD"), "abc");
}

// ---------------------------------------------------------------- 登录

#[tokio::test]
async fn 登录成功_后续请求带_bearer_token() {
    let client = reqwest::Client::new();
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(
            200,
            r#"{"access_token":"JWT-123","token_type":"bearer","auth_configured":true}"#,
        )),
        Route::get("/documents/track_status/t1").and(track_已处理("doc-1")),
        Route::post("/documents/upload").and(上传成功体("t1")),
    ])
    .await;

    let rag = 客户端(&server, &client).await;
    let _ = rag.upload_file(&上传路径(&d)).await;

    let login = server.requests_to("POST", "/login");
    assert_eq!(login.len(), 1);
    let body = login[0].body_str();
    assert!(body.contains("grant_type=password"), "要发密码模式: {body}");
    assert!(body.contains("username=u"), "{body}");
    // 密码里带特殊字符时不能被截断
    assert!(!body.contains("p&"), "表单编码要正确: {body}");

    let upload = server.requests_to("POST", "/documents/upload");
    assert_eq!(upload[0].header("authorization"), Some("Bearer JWT-123"));
}

#[tokio::test]
async fn 登录失败_把_body_里的真实原因带出来() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![Route::post("/login").and(R::json(
        401,
        r#"{"detail":"Incorrect username or password"}"#,
    ))])
    .await;

    let err = RAGRequest::new(&client, &server.base_url, "u", "bad")
        .await
        .err()
        .expect("登录失败应返回 Err")
        .to_string();
    assert!(err.contains("401"), "{err}");
    assert!(
        err.contains("Incorrect username or password"),
        "只报状态码等于没给线索: {err}"
    );
}

#[tokio::test]
async fn 登录返回非_json_报错带原文() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![
        Route::post("/login").and(R::text(200, "not json at all")),
        Route::post("/documents/upload").once(),
    ])
    .await;

    let err = RAGRequest::new(&client, &server.base_url, "u", "p")
        .await
        .err()
        .expect("非 JSON 登录响应应返回 Err")
        .to_string();
    assert!(err.contains("解析登录响应失败"), "{err}");
    assert!(err.contains("not json at all"), "{err}");
}

// ---------------------------------------------------------------- 上传 + 轮询

#[tokio::test]
async fn 上传成功_等轮询终态后返回_doc_id() {
    let client = reqwest::Client::new();
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload").and(上传成功体("track-9")),
        Route::get("/documents/track_status/track-9").and(track_已处理("doc-final")),
    ])
    .await;

    let rag = 客户端(&server, &client).await;
    let doc_id = rag.upload_file(&上传路径(&d)).await.unwrap();
    assert_eq!(doc_id, "doc-final");
    assert_eq!(server.count_to("GET", "/documents/track_status/track-9"), 1);
}

#[tokio::test]
async fn 上传的_filename_是_basename_不是全路径() {
    let client = reqwest::Client::new();
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload").and(上传成功体("t1")),
        Route::get("/documents/track_status/t1").and(track_已处理("doc-1")),
    ])
    .await;

    let rag = 客户端(&server, &client).await;
    rag.upload_file(&上传路径(&d)).await.unwrap();

    let body = server.requests_to("POST", "/documents/upload")[0].body_str();
    assert!(body.contains(r#"filename="a.md""#), "应传 basename: {body}");
    assert!(
        !body.contains("/documents/upload"),
        "不能把整条路径当文档键: {body}"
    );
}

#[tokio::test]
async fn 上传返回非_success_当失败() {
    let client = reqwest::Client::new();
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload").and(R::json(
            200,
            r#"{"status":"partial_success","message":"部分失败","track_id":"t1"}"#,
        )),
        Route::get("/documents/track_status/t1").once(),
    ])
    .await;

    let err = 客户端(&server, &client).await
        .upload_file(&上传路径(&d))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("部分失败"), "{err}");
    assert_eq!(
        server.count_to("GET", "/documents/track_status/t1"),
        0,
        "没拿到可用 track_id 就不该去轮询"
    );
}

#[tokio::test]
async fn 上传遇到_401_原样上抛_不重试() {
    let client = reqwest::Client::new();
    // 401 重试没有意义（凭据错），只会把一次失败拖成两次请求，
    // 并把真实错误掩盖成一次莫名其妙的冲突失败。
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload").and(R::json(401, r#"{"detail":"token 过期"}"#)),
        Route::post("/documents/paginated").once(),
    ])
    .await;

    let err = 客户端(&server, &client).await
        .upload_file(&上传路径(&d))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("401"), "{err}");
    assert!(err.contains("token 过期"), "{err}");
    assert_eq!(
        server.count_to("POST", "/documents/paginated"),
        0,
        "401 不该触发同名冲突清理"
    );
}

#[tokio::test]
async fn 后台处理失败_把_error_msg_带出来() {
    let client = reqwest::Client::new();
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload").and(上传成功体("t1")),
        Route::get("/documents/track_status/t1").and(R::json(
            200,
            r#"{"track_id":"t1","total_count":1,"status_summary":{"failed":1},
                "documents":[{"id":"d","status":"failed","file_path":"a.md",
                "error_msg":"Content already exists. Original doc_id: doc-old"}]}"#,
        )),
    ])
    .await;

    let err = 客户端(&server, &client).await
        .upload_file(&上传路径(&d))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Content already exists"), "{err}");
}

// 终态但 documents 为空数组的情况在这里没法测：is_finished() 对空数组恒为 false，
// _track 会一直轮询到 TRACK_TIMEOUT（900 s）才报错，测试得跑一刻钟。
// 判定逻辑本身见 tests/lightrag_model_test.rs 的 `documents_为空数组_不算完成`。

// ---------------------------------------------------------------- 409 自愈

#[tokio::test]
async fn 冲突_409_删掉旧文档后重传() {
    let client = reqwest::Client::new();
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        // 第一次上传：409；第二次：成功
        Route::post("/documents/upload")
            .and(R::json(
                409,
                r#"{"detail":"Document 'a.md' already exists in documents storage"}"#,
            ))
            .and(上传成功体("t1")),
        // 反查时旧文档还在
        Route::post("/documents/paginated")
            .and(文档页(r#"{"id":"doc-old","status":"processed","file_path":"a.md"}"#, false))
            // 删完再查：没了
            .and(文档页("", false)),
        Route::delete("/documents/delete_document").and(R::json(
            200,
            r#"{"status":"deletion_started","message":"已调度","doc_id":"doc-old"}"#,
        )),
        Route::get("/documents/track_status/t1").and(track_已处理("doc-new")),
    ])
    .await;

    let rag = 客户端(&server, &client).await;
    let doc_id = rag.upload_file(&上传路径(&d)).await.unwrap();
    assert_eq!(doc_id, "doc-new");

    // 顺序：上传(409) -> 反查 -> 删除 -> 确认消失 -> 重传 -> 轮询
    let seq: Vec<String> = server
        .requests()
        .iter()
        .filter(|r| r.target != "/login")
        .map(|r| format!("{} {}", r.method, r.target))
        .collect();
    assert_eq!(
        seq,
        vec![
            "POST /documents/upload".to_string(),
            "POST /documents/paginated".to_string(),
            "DELETE /documents/delete_document".to_string(),
            "POST /documents/paginated".to_string(),
            "POST /documents/upload".to_string(),
            "GET /documents/track_status/t1".to_string(),
        ],
        "实际请求序列:\n{}",
        server.dump()
    );

    let del = &server.requests_to("DELETE", "/documents/delete_document")[0];
    assert_eq!(del.json()["doc_ids"][0], "doc-old", "删的是反查到的那个");
    assert_eq!(del.json()["delete_file"], true);
    assert_eq!(del.json()["delete_llm_cache"], true);
}

#[tokio::test]
async fn 冲突_409_但列表查不到_报错指向_input_目录残留() {
    let client = reqwest::Client::new();
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload").and(R::json(
            409,
            r#"{"detail":"file exists in input dir"}"#,
        )),
        Route::post("/documents/paginated").and(文档页("", false)),
        Route::delete("/documents/delete_document").once(),
    ])
    .await;

    let err = 客户端(&server, &client).await
        .upload_file(&上传路径(&d))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("input"), "{err}");
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        0,
        "查不到 doc_id 时不能瞎删"
    );
}

#[tokio::test]
async fn 冲突_409_匹配到多个文档_拒绝自动删除() {
    let client = reqwest::Client::new();
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload").and(R::json(409, r#"{"detail":"dup"}"#)),
        Route::post("/documents/paginated").and(文档页(
            r#"{"id":"doc-1","status":"processed","file_path":"a.md"},
                {"id":"doc-2","status":"processed","file_path":"A.MD"}"#,
            false,
        )),
        Route::delete("/documents/delete_document").once(),
    ])
    .await;

    let err = 客户端(&server, &client).await
        .upload_file(&上传路径(&d))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("多个文档"), "{err}");
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        0,
        "多个候选时不能瞎删"
    );
}

#[tokio::test]
async fn 冲突_409_清理后仍冲突_只重试一次并带服务端信息() {
    let client = reqwest::Client::new();
    // 只重试一次：再循环下去只是把一次失败拖成无数次请求
    let (server, d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/upload")
            .and(R::json(409, r#"{"detail":"still conflicting"}"#))
            .and(R::json(409, r#"{"detail":"still conflicting again"}"#)),
        Route::post("/documents/paginated")
            .and(文档页(r#"{"id":"doc-old","status":"processed","file_path":"a.md"}"#, false))
            .and(文档页("", false)),
        Route::delete("/documents/delete_document").and(R::json(
            200,
            r#"{"status":"deletion_started","message":"ok","doc_id":"doc-old"}"#,
        )),
    ])
    .await;

    let err = 客户端(&server, &client).await
        .upload_file(&上传路径(&d))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("仍为 409"), "{err}");
    assert!(err.contains("still conflicting again"), "{err}");
    assert_eq!(
        server.count_to("POST", "/documents/upload"),
        2,
        "只允许重试一次"
    );
}

// ---------------------------------------------------------------- 删除

#[tokio::test]
async fn 删除返回_busy_报错且不做任何后续轮询() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::delete("/documents/delete_document")
            .and(R::json(200, r#"{"status":"busy","message":"pipeline 被占用"}"#)),
        Route::post("/documents/paginated").once(),
    ])
    .await;

    let err = 客户端(&server, &client)
        .await
        .delete_document("doc-1")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("busy") || err.contains("占用"), "{err}");
    assert_eq!(
        server.count_to("POST", "/documents/paginated"),
        0,
        "被拒的删除不该接着去确认"
    );
}

#[tokio::test]
async fn 删除返回未知状态_报错而不是当成功() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::delete("/documents/delete_document")
            .and(R::json(200, r#"{"status":"not_allowed","message":"无权限"}"#)),
    ])
    .await;

    let err = 客户端(&server, &client).await.delete_document("doc-1").await.unwrap_err().to_string();
    assert!(err.contains("not_allowed"), "{err}");
}

#[tokio::test]
async fn 删除已调度_要等_doc_id_从列表消失才算成功() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::delete("/documents/delete_document").and(R::json(
            200,
            r#"{"status":"deletion_started","message":"ok","doc_id":"doc-1"}"#,
        )),
        Route::post("/documents/paginated")
            .and(文档页(r#"{"id":"doc-1","status":"processed","file_path":"a.md"}"#, false))
            .and(文档页("", false)),
    ])
    .await;

    客户端(&server, &client).await.delete_document("doc-1").await.unwrap();
    assert_eq!(
        server.count_to("POST", "/documents/paginated"),
        2,
        "deletion_started 只代表已调度，必须再确认一次"
    );
}

// ---------------------------------------------------------------- 分页判定

#[tokio::test]
async fn document_exists_要翻完所有页() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/paginated")
            .and(R::json(
                200,
                r#"{"documents":[{"id":"other","status":"processed","file_path":"b.md"}],
                     "pagination":{"page":1,"total_pages":3,"total_count":250,"has_next":true}}"#,
            ))
            .and(R::json(
                200,
                r#"{"documents":[{"id":"other2","status":"processed","file_path":"c.md"}],
                     "pagination":{"page":2,"total_pages":3,"total_count":250,"has_next":true}}"#,
            ))
            .and(R::json(
                200,
                r#"{"documents":[{"id":"doc-want","status":"processed","file_path":"a.md"}],
                     "pagination":{"page":3,"total_pages":3,"total_count":250,"has_next":false}}"#,
            )),
    ])
    .await;

    let rag = 客户端(&server, &client).await;
    assert!(
        rag.document_exists("doc-want").await.unwrap(),
        "只看第一页会漏判 —— 目标在第 3 页"
    );

    let pages: Vec<u64> = server
        .requests_to("POST", "/documents/paginated")
        .iter()
        .map(|r| r.json()["page"].as_u64().unwrap())
        .collect();
    assert_eq!(pages, vec![1, 2, 3], "请求体里的 page 必须递增");
    assert_eq!(
        server.requests_to("POST", "/documents/paginated")[0].json()["page_size"],
        200
    );
}

#[tokio::test]
async fn 反查文件名_按服务端存的_basename_匹配() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/paginated").and(R::json(
            200,
            r#"{"documents":[
                  {"id":"doc-1","status":"processed","file_path":"other.md"},
                  {"id":"doc-2","status":"processed","file_path":"nested/a.md"}],
                "pagination":{"page":1,"total_pages":1,"total_count":2,"has_next":false}}"#,
        )),
    ])
    .await;

    let found = 客户端(&server, &client)
        .await
        .find_doc_ids_by_filename("a.md")
        .await
        .unwrap();
    assert_eq!(found, vec!["doc-2".to_string()], "只该匹配同名的那一条");
}

#[tokio::test]
async fn 反查文件名_大小写不同的同名文档算多条() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/paginated").and(R::json(
            200,
            r#"{"documents":[
                  {"id":"doc-1","status":"processed","file_path":"a.md"},
                  {"id":"doc-2","status":"processed","file_path":"A.MD"}],
                "pagination":{"page":1,"total_pages":1,"total_count":2,"has_next":false}}"#,
        )),
    ])
    .await;

    let found = 客户端(&server, &client)
        .await
        .find_doc_ids_by_filename("a.md")
        .await
        .unwrap();
    assert_eq!(found.len(), 2, "同名多份必须由上层决定，不能自己挑一个删");
}

#[tokio::test]
async fn 分页查询_非_2xx_报错并带_body() {
    let client = reqwest::Client::new();
    let (server, _d) = 起服务(vec![
        Route::post("/login").and(R::json(200, r#"{"access_token":"t"}"#)),
        Route::post("/documents/paginated").and(R::json(503, r#"{"detail":"维护中"}"#)),
    ])
    .await;

    let err = 客户端(&server, &client)
        .await
        .document_exists("doc-1")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("503"), "{err}");
    assert!(err.contains("维护中"), "{err}");
}
