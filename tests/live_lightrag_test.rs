//! 打真实 LightRAG 服务的端到端测试，默认全部跳过：
//! `RAG_URL=... RAG_USER=... RAG_PASS=... cargo test --test live_lightrag_test -- --nocapture --ignored`
//! 会往知识库里写文档并在结束时删掉；每个文件后台处理要 30 s 以上，别在 CI 里跑。

use rag_sync::http_util;
use rag_sync::lightrag_requests::RAGRequest;

fn credentials() -> (String, String, String) {
    (
        std::env::var("RAG_URL").expect("需要环境变量 RAG_URL"),
        std::env::var("RAG_USER").expect("需要环境变量 RAG_USER"),
        std::env::var("RAG_PASS").expect("需要环境变量 RAG_PASS"),
    )
}

/// 端到端验证「同名冲突自愈」：首次上传 -> 相同内容重传（必然 409）->
/// 改内容重传（仍走清理分支）-> 删干净。
#[tokio::test]
#[ignore = "打真实服务并写入知识库"]
async fn live_upload_conflict() -> Result<(), Box<dyn std::error::Error>> {
    let (url, user, pass) = credentials();

    let path = std::env::temp_dir().join("rag_sync_live_conflict_probe.txt");
    std::fs::write(
        &path,
        format!("live conflict probe {}\nbody\n", std::process::id()),
    )?;
    let path = path.to_string_lossy().into_owned();

    // 用带超时的客户端：reqwest::Client::new() 在主机不可达时会一直挂着
    let client = http_util::client()?;
    let rag = RAGRequest::new(&client, &url, &user, &pass).await?;

    // 1. 首次上传
    std::fs::write(
        &path,
        format!("probe round 1 {}\nbody\n", std::process::id()),
    )?;
    let first = rag.upload_file(&path).await?;
    println!("第一次上传 -> {first}");
    assert!(rag.document_exists(&first).await?);

    // 2. 相同文件重传：必然 409，走 remove_conflicting 分支。doc_id 由文件名派生，
    //    所以完全一样。
    let same = rag.upload_file(&path).await?;
    println!("重传同名 -> {same}");
    assert_eq!(first, same, "doc_id 由文件名派生，同名重传应完全相同");

    // 3. 改内容重传：仍走冲突分支，doc_id 照样不变（它只看文件名）。
    std::fs::write(
        &path,
        format!("probe round 2 DIFFERENT {}\nbody\n", std::process::id()),
    )?;
    let changed = rag.upload_file(&path).await?;
    println!("改内容重传 -> {changed}");
    assert_eq!(first, changed, "doc_id 只看文件名，内容变了也不该变");
    assert!(rag.document_exists(&changed).await?);

    // 4. 删干净
    rag.delete_document(&changed).await?;
    assert!(!rag.document_exists(&changed).await?);
    std::fs::remove_file(&path).ok();
    println!("全部通过");
    Ok(())
}
