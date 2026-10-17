
use crate::{database, db_model};
use crate::mineru_requests::mineru_tackle;
use crate::sync_config;
use crate::file_mime::SUFFIX_MIME;
use sha2::{Digest, Sha256};
use std::io::{self};
use std::{eprintln, path::{Path, PathBuf}};
use tokio;
use tokio::{fs::File as FsFile, io::AsyncReadExt};
use tokio::fs;
use walkdir::WalkDir;



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

async fn get_relative_path(root_dir: &str, file_path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    
    let root_path = Path::new(root_dir);
    let relative_path = file_path.strip_prefix(root_path).map_err(|_| {
        format!(
            "计算相对路径失败: {} 相对于 {}",
            file_path.display(),
            root_path.display()
        )
    })?;
    Ok(relative_path.to_string_lossy().into_owned())
}

async fn create_output_dir(config: &sync_config::SyncConfig, file_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let output_dir = &config.output_dir;
    let rel = get_relative_path(&config.sync_dir, file_path).await?;
    let output_path = Path::new(output_dir).join(rel);
    let _output_path = output_path.parent().ok_or("无法获取输出文件的父目录")?;
    fs::create_dir_all(&_output_path).await.map_err(|e| {
        eprintln!("错误: 创建输出目录 {} 失败: {}", _output_path.display(), e);
        "创建输出目录失败"
    })?;
    Ok(())
}

pub async fn get_files(config: &sync_config::SyncConfig) -> Result<Vec<db_model::File>, Box<dyn std::error::Error>> {
    let root_dir = &config.sync_dir;
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
                        run_id: config.run_id,
                    });
                }
            }
            Err(e) => eprintln!("错误: {}", e),
        }
    }
    Ok(files)
}

pub async fn create_output_files(config: &sync_config::SyncConfig, files: &[db_model::File],conn: &mut database::Conn) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let mut output_files: Vec<PathBuf> = Vec::new();
    for file in files {
        let file_in_db = database::db_func::file_exists(conn, &file.path).await?;
        let file_path = Path::new(&file.path);
        if file_in_db {
            let old_sha = database::db_func::get_sha256(conn, &file.path).await?;
            if old_sha.as_deref() == Some(&file.sha256) {
                println!("文件 {} 的 sha256 未变化，跳过处理", file.path);
                continue;
            }
        }
        create_output_dir(config, file_path).await?;
        let markdown = mineru_tackle(&client, file, config).await?;

        let rel = get_relative_path(&config.sync_dir, file_path).await?;
        let output_file_path = Path::new(&config.output_dir).join(rel);
        fs::write(&output_file_path, &markdown).await.map_err(|e| {
            eprintln!("错误: 写入输出文件 {} 失败: {}", output_file_path.display(), e);
            "写入输出文件失败"
        })?;
        println!("已生成输出文件: {}", output_file_path.display());
        output_files.push(output_file_path);
    }
    Ok(output_files)
}