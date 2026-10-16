mod db_model;
mod schema;
mod sync_config;
mod sync_folder;
mod file_mime;
mod mineru_requests;
mod json_model;

use diesel::{self, SqliteConnection};
use diesel_async::{AsyncConnection, sync_connection_wrapper::SyncConnectionWrapper};

use tokio;

type Conn = SyncConnectionWrapper<SqliteConnection>;

async fn establish_db_connection() -> Result<Conn, Box<dyn std::error::Error>> {
    let database_url = "rag-sync.db";
    let mut connection = Conn::establish(database_url).await?;
    Ok(connection)
}

#[tokio::main]
async fn main() {
    let sync_config = sync_config::SyncConfig::new().await.unwrap();
    match establish_db_connection().await {
        Ok(_) => {}
        Err(e) => {
            eprintln!("错误: {}", e);
            return;
        }
    }
    sync_folder::get_files(&sync_config).await.unwrap();
    println!("Sync directory: {}", &sync_config.sync_dir);
    println!("Output directory: {}", &sync_config.output_dir);
}
