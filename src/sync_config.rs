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
}
impl SyncConfig {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut file = FsFile::open("config.yml")
            .await
            .map_err(|e| format!("打开配置文件 config.yml 失败: {e}"))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .await
            .map_err(|e| format!("读取配置文件 config.yml 失败: {e}"))?;
        let config: Self =
            serde_saphyr::from_str(&contents).map_err(|e| format!("解析 config.yml 失败: {e}"))?;
        if config.sync_dir.is_empty() {
            return Err("配置文件中缺少 sync_dir 配置".into());
        }
        if config.output_dir.is_empty() {
            return Err("配置文件中缺少 output_dir 配置".into());
        }
        Ok(config)
    }
}