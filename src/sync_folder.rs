use crate::database::DBfunc;
use crate::file_mime::SUFFIX_MIME;
use crate::http_util;
use crate::lightrag_requests::{ConflictHint, RAGRequest};
use crate::mineru_requests::mineru_tackle;
use crate::sync_config;
use crate::{database, db_model};
use futures_util::stream::{FuturesUnordered, StreamExt};
use sha2::{Digest, Sha256};
use std::io::{self};
use std::{
    eprintln,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio;
use tokio::fs;
use tokio::sync::mpsc;
use tokio::{fs::File as FsFile, io::AsyncReadExt};
use walkdir::WalkDir;

pub const NOT_IN_DB: i32 = 0;

/// 中转通道容量。满了就是背压，磁盘不会被产物塞满。
const PIPELINE_BUFFER: usize = 1;

/// 直接透传、不走 MinerU 的扩展名：纯文本送进解析器只会被原样吐回来。
const PASSTHROUGH_EXTS: [&str; 3] = ["txt", "md", "markdown"];

/// 该文件走透传就返回原文，否则返回 None。
async fn passthrough_content(
    file_path: &Path,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let is_text = file_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| PASSTHROUGH_EXTS.contains(&ext.to_ascii_lowercase().as_str()));
    if !is_text {
        return Ok(None);
    }
    // 非 UTF-8 按替换字符处理，坏字节不让整个文件同步失败
    let bytes = fs::read(file_path)
        .await
        .map_err(|e| format!("读取透传文件 {} 失败: {e}", file_path.display()))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// 摘要转十六进制。
fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 字符串的 sha256，用作产物内容指纹。
pub fn sha256_text(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// 文件内容的 sha256，「文件变了没有」的判据。
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
    Ok(hex(&hasher.finalize()))
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

/// 产物命名规则：`X.ext.md`（源文件本身是 `.md` 的不加中间段），入参是相对路径。
/// 必须带原扩展名，否则同名的 `.pdf` 与 `.pptx` 会互相覆盖。
fn markdown_name(rel_path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let rel = rel_path.to_string_lossy().into_owned();
    let stem = rel_path
        .file_stem()
        .ok_or_else(|| format!("无法从 {rel} 取到文件名主干（隐藏文件?）"))?
        .to_string_lossy()
        .into_owned();
    let ext = rel_path
        .extension()
        .ok_or_else(|| format!("{rel} 没有扩展名，不该出现在扫描结果里"))?
        .to_string_lossy()
        .to_ascii_lowercase();
    let name = if ext == "md" {
        format!("{stem}.md")
    } else {
        format!("{stem}.{ext}.md")
    };
    Ok(rel_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(name)
        .to_string_lossy()
        .into_owned())
}

/// 产物在 output_dir 里的相对路径：`<别名>/<源目录内的相对路径>`。
/// 别名是多个同步目录共用一个 output_dir 时唯一的区分手段。
pub async fn output_relative_path(
    root_dir: &str,
    alias: &str,
    file_path: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let rel = get_relative_path(root_dir, file_path).await?;
    Ok(Path::new(&alias)
        .join(markdown_name(Path::new(&rel))?)
        .to_string_lossy()
        .into_owned())
}

/// 加列之前那套产物相对路径：照搬源目录结构，**不带别名那一层**。
/// `root_dir` 必须是源目录本身，否则 `join` 会得到绝对路径、删到源目录那边的文件。
pub async fn legacy_output_relative_path(
    root_dir: &str,
    file_path: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let rel = get_relative_path(root_dir, file_path).await?;
    Ok(markdown_name(Path::new(&rel))?)
}

/// 上传给 LightRAG 的文件名：`<别名>__<产物名>`，例如 `work__报告.pdf.md`。
/// LightRAG 以文件名（basename）为文档唯一键，不带别名会互相 409 并自愈删除。
pub fn upload_name(alias: &str, output_rel: &str) -> String {
    let name = Path::new(output_rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| output_rel.to_string());
    return format!("{alias}__{name}");
}

/// 某个源文件的产物相对路径；不在任何已配置目录下时返回 `None`（只可能发生在清理阶段）。
async fn output_rel_of(
    config: &sync_config::SyncConfig,
    file_path: &Path,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let Some(folder) = config.folder_of(file_path) else {
        return Ok(None);
    };
    Ok(Some(
        output_relative_path(&folder.path, &folder.name, file_path).await?,
    ))
}

/// 清理旧文件时要删的产物相对路径，最多两个候选，删掉其中存在的那个。
/// 优先 DB 里记的 `output_rel_path`；该列为 NULL（加列前行）时，新旧两套路径都试。
async fn artifact_candidates(
    config: &sync_config::SyncConfig,
    file: &db_model::File,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    if let Some(rel) = file.output_rel_path.as_deref() {
        return Ok(vec![rel.to_string()]);
    }
    let file_path = Path::new(&file.path);
    let Some(folder) = config.folder_of(file_path) else {
        return Err(format!(
            "{} 的产物路径没记录过，且它现在不在任何已配置的同步目录下（已配置别名: {}）。\
             这只可能出现在「加列之前就入库、之后又改过目录配置」的文件身上：\
             没有记录就反推不出产物位置，跳过",
            file.path,
            config.alias_list()
        )
        .into());
    };

    let mut candidates = Vec::new();
    if let Ok(rel) = output_relative_path(&folder.path, &folder.name, file_path).await {
        candidates.push(rel);
    }
    if let Ok(rel) = legacy_output_relative_path(&folder.path, file_path).await {
        if !candidates.contains(&rel) {
            candidates.push(rel);
        }
    }
    if candidates.is_empty() {
        return Err(format!("{} 算不出产物路径", file.path).into());
    }
    Ok(candidates)
}

/// 产物所在的父目录，按需创建（含别名那一层）。
async fn create_output_dir(output_file_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let parent = output_file_path
        .parent()
        .ok_or_else(|| format!("无法获取输出文件的父目录: {}", output_file_path.display()))?;
    fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("创建输出目录 {} 失败: {e}", parent.display()))?;
    Ok(())
}

pub async fn get_files(
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
) -> Result<Vec<db_model::File>, Box<dyn std::error::Error>> {
    let mut files: Vec<db_model::File> = Vec::new();
    for folder in &config.sync_folders {
        scan_folder(folder, config, conn, &mut files).await?;
    }
    // 排序保证处理顺序可复现。
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// 扫一个同步目录；目录不重叠由配置加载时的校验保证。
async fn scan_folder(
    folder: &sync_config::SyncFolder,
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
    files: &mut Vec<db_model::File>,
) -> Result<(), Box<dyn std::error::Error>> {
    let root_dir = &folder.path;

    for entry in WalkDir::new(root_dir) {
        match entry {
            Ok(e) => {
                if e.file_type().is_dir() {
                    println!("[目录][{}] {}", folder.name, e.path().display());
                } else if is_supported_file(e.path()).await {
                    let file_path = e.path();
                    println!("[文件][{}] {}", folder.name, file_path.display());

                    let path_str = file_path
                        .to_str()
                        .ok_or_else(|| format!("路径不是合法 UTF-8: {}", file_path.display()))?;
                    // sha256 一律现算：库里的值只代表上次同步时的内容。
                    let sha256 = sha256_file(path_str)
                        .await
                        .map_err(|e| format!("计算 {} 的 sha256 失败: {e}", file_path.display()))?;

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
                            output_rel_path: None,
                            uploaded_sha256: None,
                        },
                    };
                    files.push(file);
                }
            }
            Err(e) => {
                // 读不出来必须中止：漏扫一个别名等于删掉它全部的产物与知识库文档。
                let at = e
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| root_dir.clone());
                return Err(format!(
                    "扫描别名 {}({root_dir}) 下的 {at} 失败: {e}，本轮中止（跳过会把它们当成已删除）",
                    folder.name
                )
                .into());
            }
        }
    }
    Ok(())
}

/// 挑出本轮要处理的文件；没变化的也要把 run_id 刷成本轮，否则下一轮会被当成已删除。
/// 「没变化」要求内容（sha256）与产物路径都没变 —— 别名或目录挪过时必须重跑。
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
        // 没记产物路径（加列前的行）时不算「变了」，否则加列前的行会被全部重跑。
        if let Some(old_rel) = file.output_rel_path.as_deref() {
            let current_rel = output_rel_of(config, Path::new(&file.path))
                .await?
                .ok_or_else(|| {
                    format!(
                        "{} 曾经入库过（run_id={}），现在却不在任何已配置的同步目录下\
                         （已配置别名: {}）。是配置漏了这个目录？本轮中止，避免把它的产物与\
                         知识库文档当成「用户删了」清掉",
                        file.path,
                        file.run_id,
                        config.alias_list()
                    )
                })?;
            if old_rel != current_rel {
                println!(
                    "文件 {} 的产物路径已变（{old_rel} -> {current_rel}），重新处理",
                    file.path
                );
                todo.push(file);
                continue;
            }
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

/// 单个文件失败后的重试节奏：默认 5 次尝试，间隔 2s/4s/6s/8s 递增。
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// 总尝试次数（不是「重试次数」），至少为 1。
    pub max_attempts: u32,
    /// 第 n 次失败后的等待 = `base_delay * n`。
    pub base_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(2),
        }
    }
}

// 两步流水线：一次同步拆成转换（透传 / MinerU 解析）与上传（LightRAG），
// 两段并行，失败分开算 —— 上传失败不必重跑解析，解析失败不动知识库。

/// 两步之间中转的产物：转换步产出，上传步消费。
#[derive(Debug, Clone)]
struct Artifact {
    /// 源文件在库里的记录。
    file: db_model::File,
    /// 产物目录的别名，上传名的前缀。
    alias: String,
    /// 产物在 output_dir 里的相对路径（`<别名>/...`）。
    rel: String,
    /// 产物的绝对路径。
    path: PathBuf,
    /// 产物正文的 sha256。
    content_sha256: String,
    /// 在本轮待处理列表里的序号，用于稳定返回顺序。
    index: usize,
}

/// 把待同步文件逐个转成产物并写进知识库，返回产物路径。
/// 单个文件重试耗尽后立刻中止整轮，下一轮同一个 run_id 断点续跑。
pub async fn create_output_files(
    config: &sync_config::SyncConfig,
    files: &[db_model::File],
    conn: &mut database::Conn,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    create_output_files_with_retry(config, files, conn, RetryPolicy::default()).await
}

/// 同 [`create_output_files`]，重试节奏由调用方指定（测试用它把退避调成 0）。
pub async fn create_output_files_with_retry(
    config: &sync_config::SyncConfig,
    files: &[db_model::File],
    conn: &mut database::Conn,
    retry: RetryPolicy,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let todo = select_pending(config, files, conn).await?;
    // 没活干就不去连 MinerU / LightRAG。
    if todo.is_empty() {
        return Ok(Vec::new());
    }

    let client = http_util::client()?;
    let rag_request = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await?;

    let (tx, rx) = mpsc::channel::<Artifact>(PIPELINE_BUFFER);

    let (converted, uploaded) = tokio::join!(
        convert_stage(&client, config, &todo, tx, retry, config.mineru_concurrency),
        upload_stage(
            &rag_request,
            config,
            conn,
            rx,
            retry,
            config.lightrag_concurrency
        ),
    );
    converted?;
    let mut done = uploaded?;
    // 按待处理列表排序，保证返回顺序可复现。
    done.sort_by_key(|(index, _)| *index);
    Ok(done.into_iter().map(|(_, path)| path).collect())
}

/// 转换步：把待处理的文件转成产物送进 channel，攒满了就等上传步取走（背压）。
async fn convert_stage(
    client: &reqwest::Client,
    config: &sync_config::SyncConfig,
    todo: &[&db_model::File],
    tx: mpsc::Sender<Artifact>,
    retry: RetryPolicy,
    concurrency: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let limit = concurrency.max(1);
    let mut inflight = FuturesUnordered::new();
    for (index, file) in todo.iter().copied().enumerate() {
        while inflight.len() >= limit {
            let artifact = inflight.next().await.unwrap()?;
            // 上传步已经放弃，本轮到此为止。
            if tx.send(artifact).await.is_err() {
                return Ok(());
            }
        }
        inflight.push(async move {
            let mut artifact = convert_one_file_with_retry(client, config, file, retry).await?;
            artifact.index = index;
            Ok::<_, Box<dyn std::error::Error>>(artifact)
        });
    }
    while let Some(artifact) = inflight.next().await {
        if tx.send(artifact?).await.is_err() {
            break;
        }
    }
    Ok(())
}

/// 上传步：产物一到就传进知识库，传完记库，最多同时传 `concurrency` 份。
async fn upload_stage(
    rag_request: &RAGRequest<'_>,
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
    mut rx: mpsc::Receiver<Artifact>,
    retry: RetryPolicy,
    concurrency: usize,
) -> Result<Vec<(usize, PathBuf)>, Box<dyn std::error::Error>> {
    let limit = concurrency.max(1);
    let mut done: Vec<(usize, PathBuf)> = Vec::new();
    let mut inflight = FuturesUnordered::new();
    while let Some(artifact) = rx.recv().await {
        inflight.push(async move {
            let rag_id = upload_one_artifact_with_retry(rag_request, &artifact, retry).await?;
            Ok::<_, Box<dyn std::error::Error>>((artifact, rag_id))
        });
        while inflight.len() >= limit {
            let (artifact, rag_id) = inflight.next().await.unwrap()?;
            record_artifact(conn, config, &artifact, &rag_id).await?;
            done.push((artifact.index, artifact.path.clone()));
        }
    }
    // 收尾：等还在飞的上传。传成功却没记库，产物就永远失去关联。
    while let Some(result) = inflight.next().await {
        let (artifact, rag_id) = result?;
        record_artifact(conn, config, &artifact, &rag_id).await?;
        done.push((artifact.index, artifact.path.clone()));
    }
    Ok(done)
}

/// 第一步：源文件 -> markdown 产物落盘，不碰知识库、也不写库。
async fn convert_one_file(
    client: &reqwest::Client,
    config: &sync_config::SyncConfig,
    file: &db_model::File,
) -> Result<Artifact, Box<dyn std::error::Error>> {
    let file_path = Path::new(&file.path);
    // 归属别名决定产物子目录与上传名，找不到就没法落盘。
    let folder = config.folder_of(file_path).ok_or_else(|| {
        format!(
            "{} 不在任何已配置的同步目录下（已配置别名: {}）",
            file_path.display(),
            config.alias_list()
        )
    })?;
    let rel = output_relative_path(&folder.path, &folder.name, file_path).await?;
    let output_file_path = Path::new(&config.output_dir).join(&rel);
    create_output_dir(&output_file_path).await?;

    let markdown = match passthrough_content(file_path).await? {
        Some(content) => {
            println!("[透传] {} 已跳过 MinerU", file_path.display());
            content
        }
        None => mineru_tackle(client, file, config).await?,
    };

    // 指纹在写盘前算，传进知识库的就是这一份内容。
    let content_sha256 = sha256_text(&markdown);
    fs::write(&output_file_path, &markdown)
        .await
        .map_err(|e| format!("写入输出文件 {} 失败: {e}", output_file_path.display()))?;
    println!("已生成输出文件: {}", output_file_path.display());

    Ok(Artifact {
        file: file.clone(),
        alias: folder.name.clone(),
        rel,
        path: output_file_path,
        content_sha256,
        index: 0,
    })
}

/// 第一步的重试循环，失败按 `base_delay * n` 退避后从头再来。
/// 重跑是安全的：MinerU 按 sha256 幂等，产物是覆盖写。
async fn convert_one_file_with_retry(
    client: &reqwest::Client,
    config: &sync_config::SyncConfig,
    file: &db_model::File,
    retry: RetryPolicy,
) -> Result<Artifact, Box<dyn std::error::Error>> {
    // 0 次会让第一次抖动就直接失败，压成 1 次
    let max_attempts = retry.max_attempts.max(1);
    let mut attempt = 1;
    loop {
        match convert_one_file(client, config, file).await {
            Ok(artifact) => return Ok(artifact),
            Err(e) if attempt >= max_attempts => {
                return Err(format!(
                    "{} 转换失败（尝试 {attempt} 次后放弃，本轮中止）: {e}",
                    file.path
                )
                .into());
            }
            Err(e) => {
                let delay = retry.base_delay * attempt;
                println!(
                    "[重试] {} 第 {attempt}/{max_attempts} 次失败，{} 秒后重试: {e}",
                    file.path,
                    delay.as_secs()
                );
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                attempt += 1;
            }
        }
    }
}

/// 第二步：产物上传到知识库，返回 doc_id。
/// 传同名文件会 409，靠 [`ConflictHint`] 分清「内容没变（留着）」与「内容变了（删了重传）」。
async fn upload_one_artifact(
    rag_request: &RAGRequest<'_>,
    artifact: &Artifact,
) -> Result<String, Box<dyn std::error::Error>> {
    let path_str = artifact.path.to_str().ok_or_else(|| {
        format!(
            "无法将输出文件路径转换为字符串: {}",
            artifact.path.display()
        )
    })?;
    // 首次上传没有上一轮的记录，冲突时按老规矩 409。
    let hint = ConflictHint {
        uploaded_sha256: artifact.file.uploaded_sha256.as_deref(),
        rag_id: artifact.file.rag_id.as_deref(),
        content_sha256: Some(&artifact.content_sha256),
    };
    // 上传名带别名前缀：LightRAG 的唯一键是文件名，目录前缀它看不见
    let rag_id = rag_request
        .upload_file_as(
            path_str,
            &upload_name(&artifact.alias, &artifact.rel),
            Some(&hint),
        )
        .await?;
    println!("RAG 上传结果: {}", rag_id);
    Ok(rag_id)
}

/// 第二步的重试循环：只重跑上传，不重跑解析 —— 产物已经在磁盘上。
async fn upload_one_artifact_with_retry(
    rag_request: &RAGRequest<'_>,
    artifact: &Artifact,
    retry: RetryPolicy,
) -> Result<String, Box<dyn std::error::Error>> {
    // 0 次会让第一次抖动就直接失败，压成 1 次
    let max_attempts = retry.max_attempts.max(1);
    let mut attempt = 1;
    loop {
        match upload_one_artifact(rag_request, artifact).await {
            Ok(rag_id) => return Ok(rag_id),
            Err(e) if attempt >= max_attempts => {
                return Err(format!(
                    "{} 上传失败（尝试 {attempt} 次后放弃，本轮中止）: {e}",
                    artifact.file.path
                )
                .into());
            }
            Err(e) => {
                let delay = retry.base_delay * attempt;
                println!(
                    "[重试] {} 第 {attempt}/{max_attempts} 次上传失败，{} 秒后重试: {e}",
                    artifact.file.path,
                    delay.as_secs()
                );
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                attempt += 1;
            }
        }
    }
}

/// 第三步：把「这份产物已经在知识库里」记进库，必须在上传成功之后写。
async fn record_artifact(
    conn: &mut database::Conn,
    config: &sync_config::SyncConfig,
    artifact: &Artifact,
    rag_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if artifact.file.id != NOT_IN_DB {
        DBfunc::update_rag_id(conn, &artifact.file.path, Some(rag_id)).await?;
        DBfunc::update_output_rel_path(conn, &artifact.file.path, &artifact.rel).await?;
        DBfunc::update_uploaded_sha256(conn, &artifact.file.path, &artifact.content_sha256).await?;
        DBfunc::update_file_run_id(conn, &artifact.file.path, config.run_id).await?;
        DBfunc::update_sha256(conn, &artifact.file.path, &artifact.file.sha256).await?;
    } else {
        DBfunc::insert_file(
            conn,
            &artifact.file.path,
            &artifact.file.sha256,
            config.run_id,
            Some(rag_id),
            &artifact.rel,
            &artifact.content_sha256,
        )
        .await?;
    }
    Ok(())
}

/// 清理「上轮见过、这轮没扫到」的文件，只清理产物，绝不碰同步目录里的原始文档。
/// 单个失败不拖累其余，最后汇总报错、下轮重试。
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

/// 单个旧文件的清理：撤知识库 -> 删产物 -> 删 DB 行，任一步失败就保留 DB 行下轮重试。
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

    // 产物一定要删：算不出路径也不能当成「跳过」，否则产物永远失去关联。
    match artifact_candidates(config, file).await {
        Ok(rels) => {
            for rel in rels {
                let output_file_path = Path::new(&config.output_dir).join(&rel);
                match tokio::fs::remove_file(&output_file_path).await {
                    Ok(()) => println!("已删除输出文件: {}", output_file_path.display()),
                    // 已经不在了正是预期结果
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    // 权限错误不能当成「不存在」，否则会删掉 DB 行
                    Err(e) => {
                        return Err(format!(
                            "删除输出文件 {} 失败: {e}",
                            output_file_path.display()
                        )
                        .into());
                    }
                }
            }
        }
        // 路径畸形、自己恢复不了：删 DB 行，产物留下。
        Err(e) => {
            eprintln!("警告: {} 无法定位到输出目录，跳过产物清理: {e}", file.path);
        }
    };

    DBfunc::delete_file(conn, &file.path).await?;
    println!("已删除数据库记录: {}", file.path);
    Ok(())
}
