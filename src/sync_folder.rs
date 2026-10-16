
use crate::db_model;
use crate::sync_config;
use crate::file_mime::SUFFIX_MIME;
use sha2::{Digest, Sha256};
use std::io::{self};
use std::{eprintln, path::Path};
use tokio;
use tokio::{fs::File as FsFile, io::AsyncReadExt};
use tokio::fs;
use walkdir::WalkDir;

/// 由扩展名查 MIME。查不到返回 `application/octet-stream`。
/// 比较前统一转小写，避免 `REPORT.PDF` 被漏掉。
pub fn mime_for(file_path: &Path) -> &'static str {
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

async fn create_output_dir(config: &sync_config::SyncConfig, file_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let parent_path = match file_path.parent() {
        Some(p) => p,
        None => {
            eprintln!("错误: 无法获取文件 {} 的父目录", file_path.display());
            return Err("无法获取文件的父目录".into());
        }
    };
    let root_dir = &config.sync_dir;
    let output_dir = &config.output_dir;
    let rel = match parent_path.strip_prefix(&root_dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("错误: {}", e);
            return Err("无法获取相对路径".into());
        }
    };

    let _output_path = Path::new(output_dir).join(rel);
    fs::create_dir_all(&_output_path).await.map_err(|e| {
        eprintln!("错误: 创建输出目录 {} 失败: {}", _output_path.display(), e);
        "创建输出目录失败"
    })?;
    Ok(())
}

pub async fn get_files(config: &sync_config::SyncConfig) -> Result<Vec<db_model::File>, Box<dyn std::error::Error>> {
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

                    // 处理文件路径
                    println!("[文件] {}", file_path.display());
                    // 将文件信息添加到 files 向量中
                    files.push(db_model::File {
                        id: 0,
                        path: file_path.to_string_lossy().into_owned(),
                        sha256: sha256_file(file_path.to_str().unwrap())
                            .await
                            .unwrap_or_else(|_| String::new()),
                    });
                }
            }
            Err(e) => eprintln!("错误: {}", e),
        }
    }

    Ok(files)
}