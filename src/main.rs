mod db_model;
mod schema;
use serde::Deserialize;
use serde_saphyr;
use std::{eprintln, path::Path};
use walkdir::WalkDir;
use std::io::{self};
use tokio::{fs::File as FsFile, io::AsyncReadExt};
use diesel::{self, SqliteConnection};
use diesel_async::{AsyncConnection, sync_connection_wrapper::SyncConnectionWrapper};
use sha2::{Sha256, Digest};
use reqwest;
use tokio;

type Conn = SyncConnectionWrapper<SqliteConnection>;

/// 扩展名 -> MIME 类型，取自 MinerU 自身的 `MIME_TYPE_BY_EXTENSION`
/// (`mineru/filetypes.py`)。不能用各语言标准库的对照表，它们对
/// `mhtml` / `mht` / `rst` / `tex` / `ofd` / `shtml` 给的值与服务端不一致。
const SUFFIX_MIME: [(&str, &str); 25] = [
    ("pdf", "application/pdf"),
    ("ofd", "application/ofd"),
    ("epub", "application/epub+zip"),
    ("doc", "application/msword"),
    ("docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
    ("ppt", "application/vnd.ms-powerpoint"),
    ("pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
    ("xls", "application/vnd.ms-excel"),
    ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
    ("rtf", "application/rtf"),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("shtml", "text/html"),
    ("mhtml", "multipart/related"),
    ("mht", "multipart/related"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("rst", "text/x-rst"),
    ("tex", "text/x-tex"),
    ("txt", "text/plain"),
];

/// 由扩展名查 MIME。查不到返回 `application/octet-stream`。
/// 比较前统一转小写，避免 `REPORT.PDF` 被漏掉。
fn mime_for(file_path: &Path) -> &'static str {
    file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| {
            let lower = ext.to_ascii_lowercase();
            SUFFIX_MIME
                .iter()
                .find(|(suffix, _)| *suffix == lower)
                .map(|(_, mime)| *mime)
        })
        .unwrap_or("application/octet-stream")
}
#[derive(Debug, Deserialize)]
struct SyncConfig {
    #[serde(default = "String::new")]
    sync_dir: String,
    #[serde(default = "String::new")]
    output_dir: String,
    #[serde(default = "String::new")]
    mineru_url: String,
}
impl SyncConfig {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut file = FsFile::open("config.yml").await
            .map_err(|e| format!("打开配置文件 config.yml 失败: {e}"))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents).await
            .map_err(|e| format!("读取配置文件 config.yml 失败: {e}"))?;
        let config: Self = serde_saphyr::from_str(&contents)
            .map_err(|e| format!("解析 config.yml 失败: {e}"))?;
        if config.sync_dir.is_empty() {
            return Err("配置文件中缺少 sync_dir 配置".into());
        }
        if config.output_dir.is_empty() {
            return Err("配置文件中缺少 output_dir 配置".into());
        }
        Ok(config)
    }
}

async fn sha256_file(path: &str) -> io::Result<String> {
    let mut file = FsFile::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = file.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn is_supported_file(file_path: &Path) -> bool {
    file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            let lower = ext.to_ascii_lowercase();
            SUFFIX_MIME.iter().any(|(suffix, _)| *suffix == lower)
        })
}

async fn get_files(config: &SyncConfig) -> Result<Vec<db_model::File>, Box<dyn std::error::Error>> {
    let root_dir = &config.sync_dir;
    let output_dir = &config.output_dir;
    let mut files: Vec<db_model::File> = Vec::new();

    for entry in WalkDir::new(root_dir) {
        match entry {
            Ok(e) => {
                if e.file_type().is_dir() {
                    println!("[目录] {}", e.path().display());
                } else if is_supported_file(e.path()).await {
                    let file_path = e.path();
                    let parent_path = match file_path.parent() {
                        Some(p) => p,
                        None => {
                            eprintln!("错误: 无法获取文件 {} 的父目录", file_path.display());
                            return Err("无法获取文件的父目录".into());
                        }
                    };
                    let rel = match parent_path.strip_prefix(&root_dir) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("错误: {}", e);
                            return Err("无法获取相对路径".into());
                        }
                    };

                    let _output_path = Path::new(output_dir).join(rel);
                    // 处理文件路径
                    println!("[文件] {}", file_path.display());
                    // 将文件信息添加到 files 向量中
                    files.push(db_model::File {
                        id: 0,
                        path: file_path.to_string_lossy().into_owned(),
                        sha256: sha256_file(file_path.to_str().unwrap()).await.unwrap_or_else(|_| String::new()),
                    });
                }
            }
            Err(e) => eprintln!("错误: {}", e),
        }
    }

    Ok(files)
}

async fn minerU_tackle(client: reqwest::Client, file: db_model::File, config: SyncConfig) -> Result<(), Box<dyn std::error::Error>> {
  let uploads_api = "/uploads";
  client.post(format!("{}{}", config.mineru_url, uploads_api))
        
        .send().await?;
    Ok(())
}

async fn establish_db_connection() -> Result<Conn, Box<dyn std::error::Error>> {
    let database_url = "rag-sync.db";
    let mut connection = Conn::establish(database_url).await?;
    Ok(connection)
}

#[tokio::main]
async fn main() {
    let sync_config = SyncConfig::new().await.unwrap();
    match establish_db_connection().await {
        Ok(_) => {},
        Err(e) => {
            eprintln!("错误: {}", e);
            return;},
    }
    get_files(&sync_config).await.unwrap();
    println!("Sync directory: {}", &sync_config.sync_dir);
    println!("Output directory: {}", &sync_config.output_dir);
}
