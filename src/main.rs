use rag_sync::database::{self, DBfunc};
use rag_sync::{sync_config, sync_folder};

#[tokio::main]
async fn main() {
    let mut conn = match database::establish_db_connection().await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("错误: {}", e);
            return;
        }
    };
    let sync_config = sync_config::SyncConfig::new(&mut conn).await.unwrap();

    let files = sync_folder::get_files(&sync_config, &mut conn)
        .await
        .unwrap();
    sync_folder::create_output_files(&sync_config, &files, &mut conn)
        .await
        .unwrap();
    sync_folder::delete_old_files(&sync_config, &mut conn)
        .await
        .unwrap();
    DBfunc::update_run_counter(&mut conn, sync_config.run_id)
        .await
        .unwrap();
    println!("Sync directory: {}", &sync_config.sync_dir);
    println!("Output directory: {}", &sync_config.output_dir);
}
