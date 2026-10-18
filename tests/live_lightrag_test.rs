#![allow(non_snake_case)]
//! 打真实 LightRAG 服务的端到端测试，默认全部跳过。
//!
//! 需要真实的账号与网络，`cargo test` 默认不会跑；显式开启：
//!
//! ```sh
//! RAG_URL=http://192.168.2.7:9621 RAG_USER=xxx RAG_PASS=xxx \
//!   cargo test --test live_lightrag_test -- --nocapture --ignored
//! ```
//!
//! 它会往知识库里写文档并在结束时删掉，但每个文件的后台处理要 30 s 以上，
//! 所以别在 CI 里跑。

use rag_sync::lightrag_requests::RAGRequest;

fn 环境() -> (String, String, String) {
    (
        std::env::var("RAG_URL").expect("需要环境变量 RAG_URL"),
        std::env::var("RAG_USER").expect("需要环境变量 RAG_USER"),
        std::env::var("RAG_PASS").expect("需要环境变量 RAG_PASS"),
    )
}

/// 端到端验证「同名冲突自愈」。
///
/// 依次覆盖：首次上传 -> 相同内容重传（必然 409，走清理分支）->
/// 改内容重传（仍走清理分支）-> 删干净。
#[tokio::test]
#[ignore = "打真实服务并写入知识库"]
async fn live_upload_conflict() -> Result<(), Box<dyn std::error::Error>> {
    let (url, user, pass) = 环境();

    let path = std::env::temp_dir().join("rag_sync_live_conflict_probe.txt");
    std::fs::write(&path, format!("live conflict probe {}\nbody\n", std::process::id()))?;
    let path = path.to_string_lossy().into_owned();

    let client = reqwest::Client::new();
    let rag = RAGRequest::new(&client, &url, &user, &pass).await?;

    // 1. 首次上传
    std::fs::write(&path, format!("probe round 1 {}\nbody\n", std::process::id()))?;
    let first = rag.upload_file(&path).await?;
    println!("第一次上传 -> {first}");
    assert!(rag.document_exists(&first).await?);

    // 2. 相同文件重传：必然 409，走 remove_conflicting 分支。
    //    doc_id 由文件名派生（= "doc-" + md5(文件名)，已对 4 个样本核对），
    //    所以重传拿到的 doc_id 与之前完全一样。
    let same = rag.upload_file(&path).await?;
    println!("重传同名 -> {same}");
    assert_eq!(first, same, "doc_id 由文件名派生，同名重传应完全相同");

    // 3. 改内容重传：仍走冲突分支。doc_id 照样不变（它只看文件名），
    //    变的只有底下的文档内容 —— 这一点是 doc_id 派生自文件名的直接后果。
    std::fs::write(&path, format!("probe round 2 DIFFERENT {}\nbody\n", std::process::id()))?;
    let changed = rag.upload_file(&path).await?;
    println!("改内容重传 -> {changed}");
    assert_eq!(first, changed, "doc_id 只看文件名，内容变了也不该变");
    assert!(rag.document_exists(&changed).await?);

    // 4. 删干净。
    rag.delete_document(&changed).await?;
    assert!(!rag.document_exists(&changed).await?);
    std::fs::remove_file(&path).ok();
    println!("全部通过");
    Ok(())
}
