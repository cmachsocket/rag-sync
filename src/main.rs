use rag_sync::database::{self, DBfunc};
use rag_sync::{joplin_sync, sync_config, sync_folder};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// 一轮同步。返回 Err 时不推进 run_counter，下一轮会拿到同一个 run_id。
async fn run() -> Result<()> {
    let mut conn = database::establish_db_connection().await?;
    let config = sync_config::SyncConfig::new(&mut conn).await?;

    let files = sync_folder::get_files(&config, &mut conn).await?;
    sync_folder::create_output_files(&config, &files, &mut conn).await?;
    sync_folder::delete_old_files(&config, &mut conn).await?;
    DBfunc::update_run_counter(&mut conn, config.run_id).await?;

    // Joplin 是另一条独立的数据源：笔记不落盘，取到直接进知识库
    if config.joplin.is_some() {
        let summary = joplin_sync::sync_joplin(&config, &mut conn).await?;
        println!(
            "Joplin 同步: 新增 {} 篇，未变 {} 篇，删除 {} 篇，跳过 {} 个非笔记文件",
            summary.uploaded, summary.unchanged, summary.deleted, summary.skipped
        );
    }

    for folder in &config.sync_folders {
        println!("Sync directory [{}]: {}", folder.name, folder.path);
    }
    println!("Output directory: {}", config.output_dir);
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        // 非零退出码，别让 panic 栈把真正的原因埋在中间
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}
