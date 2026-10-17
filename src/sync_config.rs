use crate::database;
use serde::Deserialize;
use tokio::{fs::File as FsFile, io::AsyncReadExt};
#[derive(Debug, Deserialize)]
pub struct SyncConfig {
    #[serde(default = "String::new")]
    pub sync_dir: String,
    #[serde(default = "String::new")]
    pub output_dir: String,
    #[serde(default = "String::new")]
    pub mineru_url: String,
    #[serde(default = "zero")]
    pub run_id: i32
}

fn zero() -> i32{
    0
}

impl SyncConfig {
    pub async fn new(conn: &mut crate::database::Conn) -> Result<Self, Box<dyn std::error::Error>> {
        let mut file = FsFile::open("config.yml")
            .await
            .map_err(|e| format!("打开配置文件 config.yml 失败: {e}"))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .await
            .map_err(|e| format!("读取配置文件 config.yml 失败: {e}"))?;
        let mut config: Self =
            serde_saphyr::from_str(&contents).map_err(|e| format!("解析 config.yml 失败: {e}"))?;
        if config.sync_dir.is_empty() {
            return Err("配置文件中缺少 sync_dir 配置".into());
        }
        if config.output_dir.is_empty() {
            return Err("配置文件中缺少 output_dir 配置".into());
        }
        let old_counter: crate::db_model::RunCounter =match database::db_func::get_run_counter(conn).await? {
            Some(r) => r,
            None => return Err("无法获取运行计数器".into()),
        };
        config.run_id = old_counter.current_run_id + 1;
        Ok(config)
    }
}