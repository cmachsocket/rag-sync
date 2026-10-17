mod db_model;
mod schema;
mod sync_config;
mod sync_folder;
mod file_mime;
mod mineru_requests;
mod json_model;
mod database;



use tokio;


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
    
    let files = sync_folder::get_files(&sync_config).await.unwrap();
    sync_folder::create_output_files(&sync_config, &files,&mut conn).await.unwrap();
    println!("Sync directory: {}", &sync_config.sync_dir);
    println!("Output directory: {}", &sync_config.output_dir);
}
