use crate::database;
use serde::Deserialize;
use std::path::Path;
use tokio::{fs::File as FsFile, io::AsyncReadExt};

/// 一个同步目录。别名在两处生效，缺一不可：
/// 产物落在 `output_dir/<别名>/`，上传给 LightRAG 的文件名是 `<别名>__<文件名>`。
///
/// 别名前缀不是冗余：LightRAG 的文档唯一键是**文件名**，本地路径它只取 basename。
/// 只按别名分目录的话，`out/work/报告.pdf.md` 与 `out/personal/报告.pdf.md`
/// 上传时会撞成同一个键，409 的自愈逻辑会把先传成功的那份从知识库里删掉。
/// 别名 + 文件名即可唯一：同一别名内的重名由用户自己保证不会出现。
#[derive(Debug, Clone, Deserialize)]
pub struct SyncFolder {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub sync_folders: Vec<SyncFolder>,
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

/// 展开开头的 `~` / `~/`。不展开的话 `~/Documents` 会被当成工作目录下的相对路径：
/// 目录扫不到，DB 里存下的 path 也和实际位置对不上。
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
        // 拿不到 HOME 就原样返回，让后面的路径错误自己暴露出来
        None => path.to_string(),
    }
}

/// 别名会直接变成子目录名与上传文件名的前缀，非法字符必须在这里拦下 ——
/// 放到扫描期才发现，症状是产物被写到 output_dir 外面，或两个文件的上传名撞在一起。
fn validate_alias(name: &str) -> Result<(), String> {
    if name.contains('/') || name.contains('\\') {
        return Err("不能包含路径分隔符 / 或 \\，否则产物会写到 output_dir 之外".into());
    }
    if name == "." || name == ".." {
        return Err("不能是 . 或 ..".into());
    }
    if name.contains("__") {
        return Err(
            "不能包含 __，它是别名与文件名的分隔符：\
             别名 a__b 配文件 c.pdf 会和别名 a 配文件 b__c.pdf 撞成同一个上传名"
                .into(),
        );
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("不能包含空白字符或控制字符".into());
    }
    Ok(())
}

/// 别名互不相同、目录互不重叠，都在启动时报错，不留到扫描期再判。
///
/// 重叠的后果是同一个文件被扫两次：两份 DB 记录抢同一行、两次上传 409 互删，
/// 而「第二次扫到」会把 run_id 刷成新一轮，使第一次的清理逻辑全部失效。
fn validate_folders(folders: &[SyncFolder]) -> Result<(), Box<dyn std::error::Error>> {
    for (i, f) in folders.iter().enumerate() {
        if f.name.trim().is_empty() {
            return Err(format!("sync_folders 第 {} 项缺少别名（name）", i + 1).into());
        }
        validate_alias(&f.name)
            .map_err(|e| format!("sync_folders 第 {} 项的别名 {:?} {e}", i + 1, f.name))?;
        if f.path.trim().is_empty() {
            return Err(format!(
                "sync_folders 第 {} 项（别名 {}）缺少 path",
                i + 1,
                f.name
            )
            .into());
        }
    }

    let mut seen: Vec<(&str, &str)> = Vec::new();
    for f in folders.iter() {
        if let Some((other, _)) = seen.iter().find(|(name, _)| *name == f.name.as_str()) {
            return Err(format!(
                "别名 {} 重复配置（与别名 {other} 撞名）：别名既是产物子目录名又是上传名前缀，必须唯一",
                f.name
            )
            .into());
        }
        seen.push((f.name.as_str(), f.path.as_str()));
    }

    for (i, a) in folders.iter().enumerate() {
        for b in &folders[i + 1..] {
            let (pa, pb) = (Path::new(&a.path), Path::new(&b.path));
            if pa == pb {
                return Err(format!(
                    "别名 {} 与 {} 配了同一个目录 {}",
                    a.name, b.name, a.path
                )
                .into());
            }
            if pa.starts_with(pb) || pb.starts_with(pa) {
                return Err(format!(
                    "别名 {}（{}）与 {}（{}）的目录重叠，一个必须是另一个的祖先：\
                     重叠处的文件归属不唯一，会被扫两次",
                    a.name, a.path, b.name, b.path
                )
                .into());
            }
        }
    }
    Ok(())
}

impl SyncConfig {
    pub async fn new(conn: &mut crate::database::Conn) -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_file(conn, "config.yml").await
    }

    /// 与 `new` 相同，只是配置文件路径可指定（测试指向临时目录）。
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
        if config.sync_folders.is_empty() {
            return Err(
                "配置文件中缺少 sync_folders 配置（至少要有一个带别名的同步目录）。\
                 单目录的旧写法 sync_dir 已不再支持，改成 \
                 sync_folders:\n  - name: \"别名\"\n    path: \"原 sync_dir 的值\""
                    .into(),
            );
        }
        if config.output_dir.is_empty() {
            return Err("配置文件中缺少 output_dir 配置".into());
        }
        // 服务地址留空的话，错误会变成 reqwest 的 "builder error"，看不出缺哪个键
        for (键, 值) in [
            ("mineru_url", &config.mineru_url),
            ("lightrag_url", &config.lightrag_url),
        ] {
            if 值.is_empty() {
                return Err(format!("配置文件中缺少 {键} 配置").into());
            }
        }
        // 加载时展开：之后 path / output_dir 一定是绝对路径，
        // 存进 DB 的 path 与清理时拼回的路径才对得上。
        // 校验放在展开之后 —— 「~/work」与「/home/x/work」指同一个目录，
        // 不展开就看不出重叠。
        for folder in &mut config.sync_folders {
            folder.path = expand_home_dir(&folder.path);
        }
        config.output_dir = expand_home_dir(&config.output_dir);
        validate_folders(&config.sync_folders)?;
        let old_counter: crate::db_model::RunCounter =match database::DBfunc::get_run_counter(conn).await? {
            Some(r) => r,
            None => return Err("无法获取运行计数器".into()),
        };
        config.run_id = old_counter.current_run_id + 1;
        Ok(config)
    }

    /// 某个源文件属于哪个同步目录。取最长前缀。
    ///
    /// 配置加载时已经挡住重叠目录，这里仍挑最具体的那个：
    /// 归属不能跟着「先扫到谁」走，否则产物的子目录名会在同一台机器上时有时无。
    pub fn folder_of(&self, file_path: &Path) -> Option<&SyncFolder> {
        self.sync_folders
            .iter()
            .filter(|f| file_path.starts_with(&f.path))
            .max_by_key(|f| Path::new(&f.path).components().count())
    }

    /// 已配置的别名清单，只用在报错里 —— 「不知道该配哪个」时能一次看全。
    pub fn alias_list(&self) -> String {
        self.sync_folders
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ")
    }
}
