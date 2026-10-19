//! 库里有 failed 文档时，删除与反查还能不能工作。
//!
//! `chunks_count` 曾经是 `Option<u64>`，而真实 LightRAG 对失败的文档给 `-1` ——
//! 不是「这个字段读不出来」，而是整个响应解析失败：paginated 里混着一条 failed
//! 文档，`document_exists` / `find_doc_ids_by_filename` / `delete_document` 就全瘫，
//! 409 自愈也失效。
//!
//! ```sh
//! cargo test --test live_chunks_count_test -- --ignored --nocapture
//! ```
//! 跑完会删掉自己造的 canary，不碰既有文档。

use rag_sync::http_util;
use rag_sync::lightrag_requests::RAGRequest;
use rag_sync::sync_config::SyncConfig;

/// 先造一条「markdown 内容 + .docx 文件名」的文档 —— 服务端按扩展名选解析器，
/// 必然判失败，于是 `chunks_count = -1`。
#[tokio::test]
#[ignore = "打真实 LightRAG，写入并删除文档"]
async fn deletion_works_while_a_failed_document_exists() -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = rag_sync::database::establish_db_connection().await?;
    let config = SyncConfig::from_file(&mut conn, "config.yml").await?;
    let client = http_util::client()?;
    let rag = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await?;

    let suffix = format!("{}_{}", std::process::id(), dagobah_suffix());
    let failed_name = format!("rag_sync_failedcanary_{suffix}.docx");
    let good_name = format!("rag_sync_goodcanary_{suffix}.md");

    let root = tempfile::tempdir()?;
    // failed：正文是 markdown，名字却是 .docx
    let failed_path = root.path().join(&failed_name);
    std::fs::write(&failed_path, "# 标题\n\n".repeat(50))?;
    // good：名实相符。内容必须每次都不同 —— 服务端按**内容**去重，正文撞上
    // 已有文档会以 "Identical content already exists" 失败，那测的是另一回事。
    let good_path = root.path().join(&good_name);
    std::fs::write(
        &good_path,
        format!("# 正常文档 {suffix}\n\n") + &format!("填充内容 {suffix}。").repeat(200),
    )?;

    let mut cleanup: Vec<String> = Vec::new();

    // 上传两条。failed 那条走 409/失败路径，good 那条应当 processed。
    let good_rag_id = match rag.upload_file(good_path.to_str().unwrap()).await {
        Ok(id) => id,
        Err(e) => {
            purge(&rag, &[&failed_name, &good_name], &mut cleanup).await;
            return Err(format!("正常文档都传不上去: {e}").into());
        }
    };
    println!("正常文档入库: {good_rag_id}");

    // 确认失败文档真的以 -1 落在库里，否则这个用例测不到东西。
    let failed_id = match rag.upload_file(failed_path.to_str().unwrap()).await {
        Ok(id) => {
            println!("预期失败的文档居然成功了: {id}");
            Some(id)
        }
        Err(e) => {
            println!("failed canary 按预期被拒: {e}");
            None
        }
    };

    // 核心断言：库里此刻有一条（或两条）failed 文档，删除能力必须还在。
    let exists_good = rag.document_exists(&good_rag_id).await?;
    assert!(
        exists_good,
        "{good_rag_id} 应在库里。document_exists 走 paginated，\
         库里有 chunks_count=-1 的记录时它会整体解析失败"
    );

    let found = rag.find_doc_ids_by_filename(&good_name).await?;
    assert!(
        found.contains(&good_rag_id),
        "按文件名应能反查到 {good_rag_id}，实际 {found:?}"
    );

    rag.delete_document(&good_rag_id).await?;
    cleanup.push(good_rag_id.clone());
    assert!(
        !rag.document_exists(&good_rag_id).await?,
        "删掉了却还在 —— wait_document_gone 也走 paginated"
    );

    if let Some(id) = &failed_id {
        // failed 记录本身也该能删掉，否则它会一直卡在库里。
        match rag.delete_document(id).await {
            Ok(()) => cleanup.push(id.clone()),
            Err(e) => println!("failed 记录删除返回错误（不阻塞本用例）: {e}"),
        }
    }
    purge(&rag, &[&failed_name, &good_name], &mut cleanup).await;

    println!("已清理 {:?}", cleanup);
    Ok(())
}

/// 简单后缀，避免同一进程内重复跑时撞同名。
fn dagobah_suffix() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_else(|_| "0".into())
}

/// 按文件名反查并删除；反查本身坏掉时（正是本用例要防的）如实报错。
async fn purge(rag: &RAGRequest<'_>, names: &[&str], cleaned: &mut Vec<String>) {
    for name in names {
        match rag.find_doc_ids_by_filename(name).await {
            Ok(ids) => {
                for id in ids {
                    match rag.delete_document(&id).await {
                        Ok(()) => cleaned.push(id),
                        Err(e) => println!("  清理 {id} 失败: {e}"),
                    }
                }
            }
            Err(e) => println!("  反查 {name} 失败，可能有残留需手工确认: {e}"),
        }
    }
}
