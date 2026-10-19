use crate::database::DBfunc;
use crate::file_mime::SUFFIX_MIME;
use crate::http_util;
use crate::mineru_requests::mineru_tackle;
use crate::sync_config;
use crate::{database, db_model, lightrag_requests::RAGRequest};
use sha2::{Digest, Sha256};
use std::io::{self};
use std::{
    eprintln,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio;
use tokio::fs;
use tokio::{fs::File as FsFile, io::AsyncReadExt};
use walkdir::WalkDir;

pub const NOT_IN_DB: i32 = 0;

/// 直接透传、不走 MinerU 的扩展名。纯文本送进解析器只会被原样吐回来，
/// 却要搭上一次上传 + 解析的等待和一次失败的可能。
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
    // 非 UTF-8 按替换字符处理：坏一个字节不该让整个文件同步失败
    let bytes = fs::read(file_path)
        .await
        .map_err(|e| format!("读取透传文件 {} 失败: {e}", file_path.display()))?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

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

/// 产物的命名规则：`X.ext.md`（源文件本身是 `.md` 的不加中间段）。
/// 入参是相对路径，父目录原样保留。
///
/// - 必须 `.md`：LightRAG 按扩展名选解析器，`.docx` 里装 markdown 会被拒。
/// - 必须带原扩展名：同名的 `竞选.pdf` 与 `竞选.pptx` 都改成 `竞选.md` 会互相覆盖，
///   409 自愈还会把先传成功的那份从知识库里删掉。
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
///
/// 别名是多个同步目录共用一个 output_dir 时唯一的区分手段，没有它就分不清
/// 哪份产物属于谁。
///
/// 只依赖源路径与别名，不依赖同批次的其他文件 —— `cleanup_one_old_file` 手上只有
/// 一条 DB 记录，也必须算出同一个产物路径。
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

/// 加列之前那套产物相对路径：目录结构照搬源目录，**不带别名那一层**。
///
/// `root_dir` 必须是源目录本身：直接拿绝对路径会得到 `join` 之后整个替换掉
/// output_dir 的绝对产物路径（`Path::join` 见到绝对路径就丢掉前面的部分），
/// 那会去删源目录那边的文件。
pub async fn legacy_output_relative_path(
    root_dir: &str,
    file_path: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let rel = get_relative_path(root_dir, file_path).await?;
    Ok(markdown_name(Path::new(&rel))?)
}

/// 上传给 LightRAG 的文件名：`<别名>__<产物名>`，例如 `work__报告.pdf.md`。
///
/// LightRAG 以文件名为文档唯一键，而它只拿本地路径的 basename —— 单靠产物所在的
/// 子目录区分不开，两个目录下的同名文件会互相 409 并自愈删除。
/// 别名 + 文件名就够唯一：同一别名内的重名由用户保证不会出现。
pub fn upload_name(alias: &str, output_rel: &str) -> String {
    let name = Path::new(output_rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| output_rel.to_string());
    return format!("{alias}__{name}");
}

/// 某个源文件的产物相对路径。文件不在任何已配置目录下时返回 `None`：
/// 只可能发生在清理阶段（配置里换过目录），报错由调用方按上下文给。
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

/// 清理旧文件时要删的产物相对路径。最多两个候选，删掉其中存在的那个：
///
/// 1. DB 里记的 `output_rel_path` —— 唯一与「当时实际怎么写的」对齐的一级。
///    改了别名、挪了目录，产物也不会被找丢。命中这一级就**只有**它，
///    按当前配置现算出来的路径已经是另一个东西了。
/// 2. 加列之前入库的行（这一列是 NULL）：不知道当时写到了哪里，两个候选都试一遍 ——
///    按当前别名算的新路径，以及按旧规则（无别名目录）算的旧路径。
///    这一行的产物很可能就躺在旧位置，少试一个它就永远失去关联。
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

/// 产物所在的父目录，按需创建。目录层级来自产物相对路径，因此别名目录
/// 也会被建出来。
async fn create_output_dir(output_file_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let parent = output_file_path
        .parent()
        .ok_or_else(|| format!("无法获取输出文件的父目录: {}", output_file_path.display()))?;
    // 细节放在返回的 Err 里，不要 eprintln 一次再返回一个没原因的固定文案
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
    // 排序后处理顺序可复现，否则同一批文件在不同机器上会以不同顺序打服务端。
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// 扫一个同步目录。目录之间不重叠、不重名由配置加载时的校验保证，
/// 这里不必再判归属。
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
                    // sha256 一律现算：库里的值只说明「上次同步时是什么样」，文件改了也看不见。
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
                            // 还没处理过，产物路径不存在；等上传成功落库时再写
                            output_rel_path: None,
                        },
                    };
                    files.push(file);
                }
            }
            Err(e) => {
                // 任何一处读不出来都要中止：「没扫到」在 delete_old_files 眼里
                // 等于「用户删了」，一个权限错误就会清掉这一支的产物与知识库文档。
                // 多目录下这条更要紧：漏扫一个别名就等于删掉它全部的知识库文档。
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

/// 挑出本轮要处理的文件；没变化的也要把 run_id 刷成本轮 ——「本轮扫到过」
/// 只能靠 run_id 表达，不刷新的话下一轮会被当成用户已删除。
///
/// 「没变化」有两个条件：内容（sha256）没变，**产物路径也没变**。
/// 别名改了目录、目录挪了位置时 sha256 一样，但产物落在别处、知识库里的
/// 文档也还是旧名字 —— 那必须重跑一遍，光刷 run_id 会留下一份永远对不上的产物。
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
        // 库里没记产物路径（加列之前写入的行）时不算「变了」：那一列要等这文件
        // 真的重新处理时才会被填上，拿它判跳过会把加列前的每一行都重跑一遍。
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

/// 单个文件失败后的重试节奏。默认 5 次尝试、间隔 2s/4s/6s/8s 递增：
/// MinerU / LightRAG 的抖动（限流、pipeline 忙）多半几秒内自愈，
/// 太密的重试只是把一次故障放大成一串请求。
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// 总尝试次数（不是「重试次数」）。至少为 1。
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

/// 逐个把待同步文件转成产物并写进知识库，返回产物路径。
///
/// 单个文件按 [`RetryPolicy::default`] 重试 5 次；**耗尽后立刻中止整轮**，
/// 后面的文件本轮不再开始。中止会带着文件路径与原因上抛 -> `main` 非零退出、
/// run_counter 不推进，下一轮同一个 run_id 断点续跑（已完成的按 sha256 跳过）。
pub async fn create_output_files(
    config: &sync_config::SyncConfig,
    files: &[db_model::File],
    conn: &mut database::Conn,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    create_output_files_with_retry(config, files, conn, RetryPolicy::default()).await
}

/// 同 [`create_output_files`]，但重试节奏由调用方指定。
///
/// 拆出来是为了让测试能把退避调成 0：节奏本身只是个常量，
/// 没必要让失败用例干等 20 秒才断言「失败冒泡」。
pub async fn create_output_files_with_retry(
    config: &sync_config::SyncConfig,
    files: &[db_model::File],
    conn: &mut database::Conn,
    retry: RetryPolicy,
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
        output_files.push(
            process_one_file_with_retry(&client, &rag_request, config, conn, file, retry).await?,
        );
    }
    Ok(output_files)
}

/// 单个文件的重试循环：每次都从头跑完整链路（建目录 -> 转换 -> 写产物 ->
/// 上传 -> 落库），失败按 `base_delay * n` 退避后重来。
///
/// 重跑是安全的：MinerU 按 sha256 幂等，LightRAG 同名 409 会自愈，
/// DB 的三条更新本身幂等。
async fn process_one_file_with_retry(
    client: &reqwest::Client,
    rag_request: &RAGRequest<'_>,
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
    file: &db_model::File,
    retry: RetryPolicy,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // 0 次会让第一次抖动就直接失败，压成 1 次
    let max_attempts = retry.max_attempts.max(1);
    let mut attempt = 1;
    loop {
        match process_one_file(client, rag_request, config, conn, file).await {
            Ok(output_file_path) => return Ok(output_file_path),
            Err(e) if attempt >= max_attempts => {
                // 路径与原因都留着：CLI 与测试都靠它定位是哪个文件
                return Err(format!(
                    "{} 处理失败（尝试 {attempt} 次后放弃，本轮中止）: {e}",
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

/// 单个文件的一次完整处理，成功返回产物路径。
///
/// 落库是最后一步：LightRAG 没收到就不能记成已同步，否则会留下
/// 「库里有行、知识库没文档」的假完成状态。
async fn process_one_file(
    client: &reqwest::Client,
    rag_request: &RAGRequest<'_>,
    config: &sync_config::SyncConfig,
    conn: &mut database::Conn,
    file: &db_model::File,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let file_path = Path::new(&file.path);
    // 归属先定下来：产物的子目录名与上传名都从它来，别名找不到就没法落盘
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

    fs::write(&output_file_path, &markdown)
        .await
        .map_err(|e| format!("写入输出文件 {} 失败: {e}", output_file_path.display()))?;
    println!("已生成输出文件: {}", output_file_path.display());
    let path_str = output_file_path.to_str().ok_or_else(|| {
        format!(
            "无法将输出文件路径转换为字符串: {}",
            output_file_path.display()
        )
    })?;

    // 上传名带别名前缀：LightRAG 的唯一键是文件名，目录前缀它看不见
    let rag_id = rag_request
        .upload_file_as(path_str, &upload_name(&folder.name, &rel))
        .await?;
    println!("RAG 上传结果: {}", rag_id);
    // 记录必须在上传成功之后写：LightRAG 没收到就不能算这文件已同步
    if file.id != NOT_IN_DB {
        DBfunc::update_rag_id(conn, &file.path, Some(&rag_id)).await?;
        DBfunc::update_output_rel_path(conn, &file.path, &rel).await?;
        DBfunc::update_file_run_id(conn, &file.path, config.run_id).await?;
        DBfunc::update_sha256(conn, &file.path, &file.sha256).await?;
    } else {
        DBfunc::insert_file(
            conn,
            &file.path,
            &file.sha256,
            config.run_id,
            Some(&rag_id),
            &rel,
        )
        .await?;
    }
    Ok(output_file_path)
}

/// 清理「上轮见过、这轮没扫到」的文件。只清理我们自己造出来的东西，
/// **绝不碰同步目录里的原始文档**。
///
/// 单个失败不拖累其余，最后汇总报错：整轮非零退出、计数器不推进，下轮重试。
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

    // 产物一定要去删：删不掉就保留 DB 行下轮重试（见下面的失败分支）。
    // 「算不出路径」曾经被当成「跳过」，那会让产物永远失去关联 ——
    // 宁可删不到，也不能让这一行把清理步骤永久卡死。
    match artifact_candidates(config, file).await {
        Ok(rels) => {
            for rel in rels {
                let output_file_path = Path::new(&config.output_dir).join(&rel);
                match tokio::fs::remove_file(&output_file_path).await {
                    Ok(()) => println!("已删除输出文件: {}", output_file_path.display()),
                    // 已经不在了正是预期结果（用户自己删过产物）
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    // 权限不足之类不能当成「不存在」：那样会删掉 DB 行，产物永远失去关联。
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
        // 连文件名主干都取不出来（路径没有文件名、没有扩展名）。这一行没法自己
        // 恢复，留着只会每轮重来一次，所以仍然删 DB 行，只把产物留下。
        Err(e) => {
            eprintln!("警告: {} 无法定位到输出目录，跳过产物清理: {e}", file.path);
        }
    };

    DBfunc::delete_file(conn, &file.path).await?;
    println!("已删除数据库记录: {}", file.path);
    Ok(())
}
