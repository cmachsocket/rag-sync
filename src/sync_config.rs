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
    pub run_id: i32,
    #[serde(default = "String::new")]
    pub lightrag_password: String,
    #[serde(default = "String::new")]
    pub lightrag_url: String,
    #[serde(default = "String::new")]
    pub lightrag_username: String,
}

fn zero() -> i32{
    0
}

impl SyncConfig {
    pub async fn new(conn: &mut crate::database::Conn) -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_file(conn, "config.yml").await
    }

    /// 与 `new` 相同，只是配置文件路径可指定。生产代码用 `new`；
    /// 测试需要指向临时目录里的配置，故拆开。
    pub async fn from_file(
        conn: &mut crate::database::Conn,
        path: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut file = FsFile::open(path)
            .await
            .map_err(|e| format!("打开配置文件 {path} 失败: {e}"))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .await
            .map_err(|e| format!("读取配置文件 {path} 失败: {e}"))?;
        let mut config: Self =
            serde_saphyr::from_str(&contents).map_err(|e| format!("解析 config.yml 失败: {e}"))?;
        if config.sync_dir.is_empty() {
            return Err("配置文件中缺少 sync_dir 配置".into());
        }
        if config.output_dir.is_empty() {
            return Err("配置文件中缺少 output_dir 配置".into());
        }
        let old_counter: crate::db_model::RunCounter =match database::DBfunc::get_run_counter(conn).await? {
            Some(r) => r,
            None => return Err("无法获取运行计数器".into()),
        };
        config.run_id = old_counter.current_run_id + 1;
        Ok(config)
    }
}