use crate::database::DBfunc;
use crate::file_mime::SUFFIX_MIME;
use crate::mineru_requests::mineru_tackle;
use crate::sync_config;
use crate::{database, db_model, lightrag_requests::RAGRequest};
use sha2::{Digest, Sha256};
use std::io::{self};
use std::{
    eprintln,
    path::{Path, PathBuf},
};
use tokio;
use tokio::fs;
use tokio::{fs::File as FsFile, io::AsyncReadExt};
use walkdir::WalkDir;

pub const NOT_IN_DB: i32 = 0;

/// 供 `tests/` 直接调用：sha256 是「文件变了没有」的唯一判据，不能只测调用方。
pub async fn sha256_file(path: &str) -> io::Result<String> {
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

pub async fn is_supported_file(file_path: &Path) -> bool {
    file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            let lower = ext.to_ascii_lowercase();
            SUFFIX_MIME.iter().any(|(suffix, _)| *suffix == lower)
        })
}

pub async fn get_relative_path(
    root_dir: &str,
    file_path: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
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

async fn create_output_dir(
    config: &sync_config::SyncConfig,
    file_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
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

pub async fn get_files(
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
) -> Result<Vec<db_model::File>, Box<dyn std::error::Error>> {
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
                    let file_in_db = DBfunc::get_file(conn, file_path.to_str().unwrap()).await?;
                    // 将文件信息添加到 files 向量中
                    let file = match file_in_db {
                        Some(file) => file,
                        None => db_model::File {
                            id: NOT_IN_DB,
                            path: file_path.to_string_lossy().into_owned(),
                            sha256: sha256_file(file_path.to_str().unwrap())
                                .await
                                .unwrap_or_else(|_| String::new()),
                            run_id: config.run_id,
                            rag_id: None, // 初始化为 None，稍后可以根据需要更新
                        },
                    };
                    files.push(file);
                }
            }
            Err(e) => eprintln!("错误: {}", e),
        }
    }
    Ok(files)
}

pub async fn create_output_files(
    config: &sync_config::SyncConfig,
    files: &[db_model::File],
    conn: &mut database::Conn,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let mut output_files: Vec<PathBuf> = Vec::new();
    let rag_request = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await?;
    for file in files {
        let file_path = Path::new(&file.path);
        if file.id != NOT_IN_DB {
            let old_sha = DBfunc::get_sha256(conn, &file.path).await?;
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
            eprintln!(
                "错误: 写入输出文件 {} 失败: {}",
                output_file_path.display(),
                e
            );
            "写入输出文件失败"
        })?;
        println!("已生成输出文件: {}", output_file_path.display());
        output_files.push(output_file_path.clone());
        match output_file_path.to_str() {
            Some(path_str) => {
                let result = rag_request.upload_file(path_str).await?;
                println!("RAG 上传结果: {}", result);
                // 更新数据库中的 rag_id
                if file.id != NOT_IN_DB {
                    DBfunc::update_rag_id(conn, &file.path, Some(&result)).await?;
                    DBfunc::update_file_run_id(conn, &file.path, config.run_id).await?;
                    DBfunc::update_sha256(conn, &file.path, &file.sha256).await?;
                } else {
                    // 如果文件不在数据库中，插入新记录
                    DBfunc::insert_file(
                        conn,
                        &file.path,
                        &file.sha256,
                        config.run_id,
                        Some(&result),
                    )
                    .await?;
                }
            }
            None => {
                eprintln!(
                    "错误: 无法将输出文件路径转换为字符串: {}",
                    output_file_path.display()
                );
            }
        }
    }
    Ok(output_files)
}
pub async fn delete_old_files(
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
) -> Result<(), Box<dyn std::error::Error>> {
    let old_files = DBfunc::found_file_less_than_run_id(conn, config.run_id).await?;
    for file in old_files {
        let file_path = Path::new(&file.path);
        if file_path.exists() {
            fs::remove_file(file_path).await.map_err(|e| {
                eprintln!("错误: 删除旧文件 {} 失败: {}", file_path.display(), e);
                "删除旧文件失败"
            })?;
            println!("已删除旧文件: {}", file_path.display());
        }
    }
    Ok(())
}