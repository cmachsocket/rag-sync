use crate::database::DBfunc;
use crate::http_util;
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
    let rel = get_relative_path(&config.sync_dir, file_path).await?;
    let output_file_path = Path::new(&config.output_dir).join(rel);
    let parent = output_file_path.parent().ok_or_else(|| {
        format!("无法获取输出文件的父目录: {}", output_file_path.display())
    })?;
    // 错误信息里带一次详细信息即可：eprintln + 固定文案会让上层拿到
    // 一条没有原因的错误，真正的原因只留在 stderr 里。
    fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("创建输出目录 {} 失败: {e}", parent.display()))?;
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
                    // sha256 一律现算：库里的值只说明「上次同步时是什么样」，文件改了也看不见。
                    let sha256 = sha256_file(path_str).await.map_err(|e| {
                        format!("计算 {} 的 sha256 失败: {e}", file_path.display())
                    })?;

                    let file = match DBfunc::get_file(conn, path_str).await? {
                        Some(mut file) => {
                            // 库里的 id / rag_id / run_id 留用，只换 sha256
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
                // 任何一处读不出来都必须让整轮停下来：「没扫到」在 delete_old_files
                // 眼里等于「用户把它删了」，一个权限错误就会清掉这一支的产物和知识库文档。
                let at = e
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| root_dir.clone());
                return Err(format!("扫描 {at} 失败: {e}，本轮中止（跳过会把它们当成已删除）").into());
            }
        }
    }
    // 排序后处理顺序可复现，否则同一批文件在不同机器上会以不同顺序打服务端。
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// 挑出本轮真正要处理的文件，顺带把没变化的文件的 run_id 刷成本轮。
///
/// 「本轮扫到过」只能靠刷新 run_id 表达（delete_old_files 靠 run_id < 本轮 判定
/// 「上轮见过、这轮没了」）。跳过的文件不刷新，下一轮就会被当成用户已删除。
async fn select_pending<'a>(
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
    let todo = select_pending(config, files, conn).await?;
    let mut output_files: Vec<PathBuf> = Vec::new();
    // 没有要处理的文件就不去连 MinerU / LightRAG：服务不可达不该让「没事可做」变成失败。
    if todo.is_empty() {
        return Ok(output_files);
    }

    let client = http_util::client()?;
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

/// 清理「上轮见过、这轮没扫到」的文件。只清理我们自己造出来的东西，
/// **绝不碰同步目录里的原始文档**。
///
/// 单个文件失败不拖累其余，最后汇总成一个错误返回：整轮会非零退出、
/// 计数器不推进，下一轮重试这些条目。
pub async fn delete_old_files(
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
) -> Result<(), Box<dyn std::error::Error>> {
    let old_files = DBfunc::found_file_less_than_run_id(conn, config.run_id).await?;
    if old_files.is_empty() {
        return Ok(());
    }

    let client = http_util::client()?;
    let rag_request = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await?;

    let mut failures: Vec<String> = Vec::new();
    for file in old_files {
        if let Err(e) = cleanup_one_old_file(&rag_request, config, conn, &file).await {
            failures.push(format!("{}: {e}", file.path));
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    Err(format!(
        "清理 {} 个旧文件失败（其余已清理干净）:\n  - {}",
        failures.len(),
        failures.join("\n  - ")
    )
    .into())
}

/// 单个旧文件的清理：撤知识库 -> 删产物 -> 删 DB 行。
/// 任一步失败就保留 DB 行下轮重试，否则会留下无主状态。
async fn cleanup_one_old_file(
    rag_request: &RAGRequest<'_>,
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
    file: &db_model::File,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(rag_id) = file.rag_id.as_deref() {
        rag_request
            .delete_document(rag_id)
            .await
            .map_err(|e| format!("清理知识库文档 {rag_id} 失败: {e}"))?;
        println!("已删除知识库文档: {rag_id}");
    }

    // 路径定位不到输出目录（通常用户改过 sync_dir）：跳过产物清理，
    // 但知识库与 DB 行照清，否则这一行会把整个清理步骤永久卡死。
    match get_relative_path(&config.sync_dir, Path::new(&file.path)).await {
        Ok(rel) => {
            let output_file_path = Path::new(&config.output_dir).join(rel);
            match tokio::fs::remove_file(&output_file_path).await {
                Ok(()) => println!("已删除输出文件: {}", output_file_path.display()),
                // 已经不在了正是预期结果（用户自己删过产物）
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                // 权限不足之类不能当成「不存在」：那样会删掉 DB 行，
                // 产物从此永久失去关联、再也找不到。
                Err(e) => {
                    return Err(format!(
                        "删除输出文件 {} 失败: {e}",
                        output_file_path.display()
                    )
                    .into())
                }
            }
        }
        Err(e) => eprintln!("警告: {} 无法定位到输出目录，跳过产物清理: {e}", file.path),
    }

    DBfunc::delete_file(conn, &file.path).await?;
    println!("已删除数据库记录: {}", file.path);
    Ok(())
}