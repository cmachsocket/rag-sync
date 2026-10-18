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

fn zero() -> i32 {
    0
}

/// 展开开头的 `~` / `~/`。
///
/// 不展开的话，`sync_dir: "~/Documents"` 会被当成工作目录下一个叫
/// `~/Documents` 的相对路径：目录扫不到（这轮等于什么都没干），更糟的是
/// DB 里存下的 path 与实际位置对不上，历史记录全成孤儿。
fn expand_home_dir(path: &str) -> String {
    let rest = if path == "~" {
        ""
    } else if let Some(rest) = path.strip_prefix("~/") {
        rest
    } else {
        return path.to_string();
    };
    match std::env::var_os("HOME") {
        Some(home) if rest.is_empty() => home.to_string_lossy().into_owned(),
        Some(home) => {
            std::path::Path::new(&home)
                .join(rest)
                .to_string_lossy()
                .into_owned()
        }
        // 拿不到 HOME 就原样返回，让后面的路径错误自己暴露出来，
        // 总比悄悄展开成别处强。
        None => path.to_string(),
    }
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
        // 服务地址在用到时才拼 URL，留空的话错误会变成 reqwest 的 "builder error"，
        // 完全看不出是哪个键没配。在这里挡住。
        for (键, 值) in [
            ("mineru_url", &config.mineru_url),
            ("lightrag_url", &config.lightrag_url),
        ] {
            if 值.is_empty() {
                return Err(format!("配置文件中缺少 {键} 配置").into());
            }
        }
        // 统一在加载时展开：之后 sync_dir / output_dir 一定是绝对路径，
        // get_files 存进 DB 的 path 与 delete_old_files 拼回的路径才对得上。
        config.sync_dir = expand_home_dir(&config.sync_dir);
        config.output_dir = expand_home_dir(&config.output_dir);
        let old_counter: crate::db_model::RunCounter =match database::DBfunc::get_run_counter(conn).await? {
            Some(r) => r,
            None => return Err("无法获取运行计数器".into()),
        };
        config.run_id = old_counter.current_run_id + 1;
        Ok(config)
    }
}