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
                    println!("[文件] {}", file_path.display());

                    let path_str = file_path.to_str().ok_or_else(|| {
                        format!("路径不是合法 UTF-8: {}", file_path.display())
                    })?;
                    // sha256 一律现算。库里的 sha256 只能说明「上次同步时是什么样」，
                    // 拿它和自己比永远相等，文件改了也看不见。
                    let sha256 = sha256_file(path_str).await.map_err(|e| {
                        format!("计算 {} 的 sha256 失败: {e}", file_path.display())
                    })?;

                    let file = match DBfunc::get_file(conn, path_str).await? {
                        Some(mut file) => {
                            // 库里的 id / rag_id / run_id 要留用，只有 sha256 换成磁盘上的实况
                            file.sha256 = sha256;
                            file
                        }
                        None => db_model::File {
                            id: NOT_IN_DB,
                            path: file_path.to_string_lossy().into_owned(),
                            sha256,
                            run_id: config.run_id,
                            rag_id: None,
                        },
                    };
                    files.push(file);
                }
            }
            Err(e) => {
                // 根目录本身就打不开（路径写错、没权限）必须让整轮失败 ——
                // 与「目录里没有可同步的文件」区分开，否则计数器会白白推进一轮。
                // 子目录读不了只丢那一支，继续扫剩下的。
                if e.depth() == 0 {
                    return Err(format!(
                        "扫描同步目录 {} 失败: {e}",
                        Path::new(root_dir).display()
                    )
                    .into());
                }
                eprintln!("警告: 跳过 {:?}: {e}", e.path());
            }
        }
    }
    // WalkDir 按文件系统顺序返回。排序后处理顺序可复现，
    // 否则同一批文件在不同机器上会以不同顺序打服务端。
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// 挑出本轮真正要处理的文件，顺带把没变化的文件的 run_id 刷成本轮。
///
/// 「本轮扫到过」这个信号只能靠刷新 run_id 表达：delete_old_files 靠
/// run_id < 本轮 判定「上轮见过、这轮没了」。跳过的文件不刷新，
/// 下一轮就会被当成用户已删除而连带清掉它的产物与知识库条目。
async fn 待处理<'a>(
    config: &sync_config::SyncConfig,
    files: &'a [db_model::File],
    conn: &mut database::Conn,
) -> Result<Vec<&'a db_model::File>, Box<dyn std::error::Error>> {
    let mut todo = Vec::new();
    for file in files {
        if file.id == NOT_IN_DB {
            todo.push(file);
            continue;
        }
        let old_sha = DBfunc::get_sha256(conn, &file.path).await?;
        if old_sha.as_deref() == Some(file.sha256.as_str()) {
            println!("文件 {} 的 sha256 未变化，跳过处理", file.path);
            DBfunc::update_file_run_id(conn, &file.path, config.run_id).await?;
            continue;
        }
        todo.push(file);
    }
    Ok(todo)
}

pub async fn create_output_files(
    config: &sync_config::SyncConfig,
    files: &[db_model::File],
    conn: &mut database::Conn,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let todo = 待处理(config, files, conn).await?;
    let mut output_files: Vec<PathBuf> = Vec::new();
    // 没有要处理的文件就不去连 MinerU / LightRAG。服务不可达不该让
    // 「这轮没事可做」变成一轮失败。
    if todo.is_empty() {
        return Ok(output_files);
    }

    let client = reqwest::Client::new();
    let rag_request = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await?;
    for file in todo {
        let file_path = Path::new(&file.path);
        create_output_dir(config, file_path).await?;
        let markdown = mineru_tackle(&client, file, config).await?;

        let rel = get_relative_path(&config.sync_dir, file_path).await?;
        let output_file_path = Path::new(&config.output_dir).join(rel);
        fs::write(&output_file_path, &markdown).await.map_err(|e| {
            format!("写入输出文件 {} 失败: {e}", output_file_path.display())
        })?;
        println!("已生成输出文件: {}", output_file_path.display());
        output_files.push(output_file_path.clone());
        let path_str = output_file_path.to_str().ok_or_else(|| {
            format!(
                "无法将输出文件路径转换为字符串: {}",
                output_file_path.display()
            )
        })?;

        let rag_id = rag_request.upload_file(path_str).await?;
        println!("RAG 上传结果: {}", rag_id);
        // 记录必须在上传成功之后写：LightRAG 没收到就不能算这文件已同步
        if file.id != NOT_IN_DB {
            DBfunc::update_rag_id(conn, &file.path, Some(&rag_id)).await?;
            DBfunc::update_file_run_id(conn, &file.path, config.run_id).await?;
            DBfunc::update_sha256(conn, &file.path, &file.sha256).await?;
        } else {
            DBfunc::insert_file(conn, &file.path, &file.sha256, config.run_id, Some(&rag_id))
                .await?;
        }
    }
    Ok(output_files)
}

/// 清理「上轮见过、这轮没扫到」的文件。
///
/// 只清理我们自己造出来的东西，**绝不碰同步目录里的原始文档** ——
/// 那是用户的文件，本程序只读。顺序也有讲究：先从知识库里撤掉，
/// 再删产物，最后才删 DB 行。反过来做的话，中途失败会留下无主条目。
pub async fn delete_old_files(
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
) -> Result<(), Box<dyn std::error::Error>> {
    let old_files = DBfunc::found_file_less_than_run_id(conn, config.run_id).await?;
    if old_files.is_empty() {
        return Ok(());
    }

    let client = reqwest::Client::new();
    let rag_request = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await?;

    for file in old_files {
        if let Some(rag_id) = file.rag_id.as_deref() {
            rag_request.delete_document(rag_id).await.map_err(|e| {
                format!("清理知识库文档 {rag_id}（{}）失败: {e}", file.path)
            })?;
            println!("已删除知识库文档: {rag_id}");
        }
        let rel = match get_relative_path(&config.sync_dir, Path::new(&file.path)).await {
            Ok(rel) => rel,
            Err(e) => {
                // 记录里的路径不在当前 sync_dir 之下 —— 通常是用户改过配置。
                // 知识库和 DB 行照常清掉，否则清理这一步会被这一行永久卡死。
                eprintln!("警告: {} 无法定位到输出目录，跳过产物清理: {e}", file.path);
                DBfunc::delete_file(conn, &file.path).await?;
                println!("已删除数据库记录: {}", file.path);
                continue;
            }
        };
        let output_file_path = Path::new(&config.output_dir).join(rel);
        if tokio::fs::try_exists(&output_file_path).await.unwrap_or(false) {
            tokio::fs::remove_file(&output_file_path).await.map_err(|e| {
                format!("删除输出文件 {} 失败: {e}", output_file_path.display())
            })?;
            println!("已删除输出文件: {}", output_file_path.display());
        }
        DBfunc::delete_file(conn, &file.path).await?;
        println!("已删除数据库记录: {}", file.path);
    }
    Ok(())
}