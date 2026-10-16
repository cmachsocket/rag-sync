use serde::Deserialize;
use serde_saphyr;
use std::{eprintln, path::Path};
use walkdir::WalkDir;
use std::io::{self, Read};
use std::fs::File;
use md5::{Md5, Digest};

const SUFFIX: [&str; 26] = [
    "pdf", "ofd", "epub", "doc", "docx", "ppt", "pptx", "xls", "xlsx", "rtf", "odt", "ods", "odp",
    "html", "htm", "shtml", "mhtml", "mht", "csv", "tsv", "md", "markdown", "rst", "tex", "txt",
    "rs",
];
#[derive(Debug, Deserialize)]
struct SyncConfig {
    #[serde(default = "String::new")]
    sync_dir: String,
    #[serde(default = "String::new")]
    output_dir: String,
}
impl SyncConfig {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let file = std::fs::File::open("config.yml")
            .map_err(|e| format!("打开配置文件 config.yml 失败: {e}"))?;
        let mut config: Self =
            serde_saphyr::from_reader(file).map_err(|e| format!("解析 config.yml 失败: {e}"))?;
        if config.sync_dir.is_empty() {
            return Err("配置文件中缺少 sync_dir 配置".into());
        }
        if config.output_dir.is_empty() {
            return Err("配置文件中缺少 output_dir 配置".into());
        }
        Ok(config)
    }
}

fn md5_file(path: &str) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Md5::new();
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    let digest = hasher.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn is_supported_file(file_path: &Path) -> bool {
    if let Some(ext) = file_path.extension() {
        if let Some(ext_str) = ext.to_str() {
            return SUFFIX.contains(&ext_str);
        }
    }
    false
}

fn sync_dir(config: SyncConfig) -> Result<(), Box<dyn std::error::Error>> {
    let root_dir = &config.sync_dir;
    let output_dir = &config.output_dir;
    Ok(WalkDir::new(root_dir)
        .into_iter()
        .for_each(|entry| match entry {
            Ok(e) => {
                if e.file_type().is_dir() {
                    println!("[目录] {}", e.path().display());
                } else if is_supported_file(e.path()) {
                    let parent_path = match e.path().parent() {
                        Some(p) => p,
                        None => {
                            eprintln!("错误: 无法获取文件 {} 的父目录", e.path().display());
                            return;
                        }
                    };
                    let rel = match parent_path.strip_prefix(&root_dir) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("错误: {}", e);
                            return;
                        }
                    };

                    let dest = Path::new(output_dir).join(rel);
                    println!("[文件] {}", e.path().display());
                }
            }
            Err(e) => eprintln!("错误: {}", e),
        }))
}

fn main() {
    
    let config = SyncConfig::new().unwrap();
    println!("Sync directory: {}", config.sync_dir);
    println!("Output directory: {}", config.output_dir);
}
