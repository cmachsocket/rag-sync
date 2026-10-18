//! LightRAG 响应体的反序列化与状态判定，样本取自
//! lightrag-upload-endpoint-test.md 记录的实测报文。

use rag_sync::lightrag_model::*;

// ---------------------------------------------------------------- 登录

#[test]
fn login_response_full_body() {
    let r: LoginResponse = serde_json::from_str(
        r#"{
          "access_token": "<JWT, 长度 199>",
          "token_type": "bearer",
          "auth_configured": true,
          "auth_mode": "enabled",
          "core_version": "1.5.7",
          "api_version": "0344"
        }"#,
    )
    .unwrap();
    assert_eq!(r.access_token, "<JWT, 长度 199>");
    assert_eq!(r.token_type, "bearer");
    assert!(r.auth_configured);
    assert_eq!(r.core_version, "1.5.7");
}

#[test]
fn login_response_token_only() {
    // 服务端加字段不该让登录整体失效
    let r: LoginResponse = serde_json::from_str(r#"{"access_token":"abc"}"#).unwrap();
    assert_eq!(r.access_token, "abc");
    assert_eq!(r.token_type, "");
    assert!(!r.auth_configured);
}

#[test]
fn login_response_without_token_is_error() {
    // 空 token 会让后续请求以 401 失败，把登录问题伪装成接口问题
    let res = serde_json::from_str::<LoginResponse>(r#"{"token_type":"bearer"}"#);
    assert!(res.is_err(), "缺 access_token 时必须报错");
    let e = res.err().unwrap();
    assert!(e.to_string().contains("access_token"), "{e}");
}

// ---------------------------------------------------------------- track_status

fn doc(id: &str, status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "status": status,
        "file_path": format!("{id}.md"),
    })
}

fn track(docs: serde_json::Value) -> TrackResponse {
    serde_json::from_str(&serde_json::json!({
        "track_id": "upload_20261017_002603_f93bcf1d",
        "documents": docs,
        "total_count": 1,
        "status_summary": {"processing": 1}
    })
    .to_string())
    .unwrap()
}

#[test]
fn track_envelope_has_no_top_level_status() {
    let t = track(serde_json::json!([doc("doc-1", "processing")]));
    // 顶层只有 track_id / documents / total_count / status_summary
    assert_eq!(t.track_id, "upload_20261017_002603_f93bcf1d");
    assert_eq!(t.documents.len(), 1);
    assert_eq!(t.total_count, 1);
    assert_eq!(t.status_summary.get("processing"), Some(&1));
}

#[test]
fn pending_is_not_finished() {
    for st in ["pending", "parsing", "analyzing", "processing", "preprocessed"] {
        let t = track(serde_json::json!([doc("doc-1", st)]));
        assert!(!t.is_finished(), "{st} 被当成了终态");
    }
}

#[test]
fn processed_is_terminal_in_any_case() {
    // OpenAPI 示例给 "PROCESSED"，实测响应给 "processed"
    assert!(track(serde_json::json!([doc("doc-1", "processed")])).is_finished());
    assert!(track(serde_json::json!([doc("doc-1", "PROCESSED")])).is_finished());
}

#[test]
fn failed_is_terminal() {
    let t = track(serde_json::json!([doc("doc-1", "failed")]));
    assert!(t.is_finished(), "failed 是终态，不该一直轮询下去");
}

#[test]
fn empty_documents_is_not_finished() {
    // 拿到 track_id 的瞬间查列表可能返回空数组，和「文档已被删除」响应一模一样。
    // 去掉这个判断，`all()` 在空迭代器上恒为 true，会立刻误判完成。
    let t = track(serde_json::json!([]));
    assert!(!t.is_finished());
    assert_eq!(t.documents.len(), 0);
}

#[test]
fn all_documents_must_be_terminal() {
    let done = track(serde_json::json!([doc("a", "processed"), doc("b", "processed")]));
    assert!(done.is_finished());
    let half = track(serde_json::json!([doc("a", "processed"), doc("b", "processing")]));
    assert!(!half.is_finished(), "还有一个在跑就不能算完成");
}

#[test]
fn first_doc_id_takes_head() {
    let t = track(serde_json::json!([doc("doc-1", "processed"), doc("doc-2", "processed")]));
    assert_eq!(t.first_doc_id(), Some("doc-1"));
    let empty = track(serde_json::json!([]));
    assert_eq!(empty.first_doc_id(), None);
}

#[test]
fn first_error_reads_dedup_message() {
    let t: TrackResponse = serde_json::from_str(
        r#"{
          "track_id": "t1",
          "total_count": 1,
          "status_summary": {"failed": 1},
          "documents": [{
            "id": "doc-1",
            "status": "failed",
            "file_path": "a.md",
            "error_msg": "Content already exists. Original doc_id: doc-abc"
          }]
        }"#,
    )
    .unwrap();
    assert_eq!(
        t.first_error(),
        Some("Content already exists. Original doc_id: doc-abc")
    );
}

#[test]
fn first_error_is_case_insensitive() {
    let t: TrackResponse = serde_json::from_str(
        r#"{"track_id":"t","total_count":1,"status_summary":{},
            "documents":[{"id":"d","status":"FAILED","file_path":"a.md","error_msg":"boom"}]}"#,
    )
    .unwrap();
    assert_eq!(t.first_error(), Some("boom"));
}

#[test]
fn first_error_none_when_absent() {
    let t = track(serde_json::json!([doc("doc-1", "failed")]));
    assert_eq!(t.first_error(), None);
}

#[test]
fn first_error_none_on_success() {
    let t = track(serde_json::json!([doc("doc-1", "processed")]));
    assert_eq!(t.first_error(), None);
}

#[test]
fn metadata_parses_across_stages() {
    // 处理中：chunk_method / chunk_opts / process_options
    // 终态：只剩 parse_engine / parse_format / mm_chunks / kg_write_state
    for meta in [
        serde_json::json!({
            "parse_engine": "native", "parse_format": "lightrag",
            "chunk_method": "paragraph_semantic", "chunk_opts": "size=2000, overlap=100",
            "mm_chunks": 4, "kg_write_state": "pre_graph", "process_options": "teP"
        }),
        serde_json::json!({"parse_engine": "native", "mm_chunks": 0, "kg_write_state": "done"}),
        serde_json::json!({}),
    ] {
        let s = serde_json::json!({
            "id": "d", "status": "processed", "file_path": "a.md", "metadata": meta
        });
        let r: DocStatusResponse = serde_json::from_str(&s.to_string()).unwrap();
        assert_eq!(r.id, "d");
    }
}

#[test]
fn missing_optional_fields_default_to_none() {
    let r: DocStatusResponse =
        serde_json::from_str(r#"{"id":"d","status":"pending","file_path":"a.md"}"#).unwrap();
    assert_eq!(r.chunks_count, None);
    assert_eq!(r.error_msg, None);
    assert_eq!(r.content_length, None);
    assert_eq!(r.metadata, None);
}

// ---------------------------------------------------------------- 删除

#[test]
fn delete_response_three_states() {
    let started: DeleteDocByIdResponse = serde_json::from_str(
        r#"{"status":"deletion_started","message":"...","doc_id":"doc-1"}"#,
    )
    .unwrap();
    assert!(started.is_scheduled());
    assert!(!started.is_busy());

    let busy: DeleteDocByIdResponse =
        serde_json::from_str(r#"{"status":"busy","message":"pipeline 忙"}"#).unwrap();
    assert!(!busy.is_scheduled());
    assert!(busy.is_busy(), "200 + busy 是「被拒且无副作用」，不能当成功");

    let not_allowed: DeleteDocByIdResponse =
        serde_json::from_str(r#"{"status":"not_allowed","message":"无权限"}"#).unwrap();
    assert!(!not_allowed.is_scheduled());
    assert!(!not_allowed.is_busy());
}

#[test]
fn delete_response_without_optional_fields() {
    let r: DeleteDocByIdResponse = serde_json::from_str(r#"{"status":"deletion_started"}"#).unwrap();
    assert!(r.is_scheduled());
    assert_eq!(r.message, "");
    assert_eq!(r.doc_id, "");
}

#[test]
fn delete_request_body_flags() {
    let req = DeleteDocRequest {
        doc_ids: vec!["doc-1".into()],
        delete_file: true,
        delete_llm_cache: true,
    };
    assert_eq!(
        serde_json::to_string(&req).unwrap(),
        r#"{"doc_ids":["doc-1"],"delete_file":true,"delete_llm_cache":true}"#
    );
}

// ---------------------------------------------------------------- 分页

#[test]
fn first_page_uses_max_page_size() {
    let r = DocumentsRequest::first_page();
    assert_eq!(r.page, 1);
    assert_eq!(r.page_size, 200, "服务端上限是 200，超了会 422");
    assert!(!serde_json::to_string(&r).unwrap().contains("\"page\":0"));
}

#[test]
fn missing_pagination_means_no_next_page() {
    // 少给 has_next 时必须停在第一页，不能当成「还有下一页」死循环
    let r: PaginatedDocsResponse =
        serde_json::from_str(r#"{"documents":[],"pagination":{}}"#).unwrap();
    assert!(!r.pagination.has_next);
    assert_eq!(r.pagination.total_pages, 0);
    assert!(r.documents.is_empty());
}

#[test]
fn upload_response_non_success_status() {
    let r: UploadResponse = serde_json::from_str(
        r#"{"status":"failure","message":"bad file","track_id":""}"#,
    )
    .unwrap();
    assert_eq!(r.status, "failure");
    assert_eq!(r.message, "bad file");
}
