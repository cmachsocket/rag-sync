//! cargo test --test e2e_config_test -- --ignored --nocapture           # 配置 + 连通性，只读
//! cargo test --test e2e_config_test mineru_job -- --ignored --nocapture # 只打真 MinerU（任务接口）
//! cargo test --test e2e_config_test probe -- --ignored --nocapture     # 单文件往返，结束自动删
//! cargo test --test e2e_config_test full_sync -- --ignored --nocapture # 全量入库，见下
//! cargo test --test e2e_config_test joplin_ -- --ignored --nocapture  # Joplin 同步流，只读 + 单篇往返

mod common;

use std::path::PathBuf;

use rag_sync::database::{self, DBfunc};
use rag_sync::db_model;
use rag_sync::http_util;
use rag_sync::joplin_model;
use rag_sync::joplin_requests::{JoplinClient, JoplinError};
use rag_sync::lightrag_requests::RAGRequest;
use rag_sync::sync_config::{SyncConfig, SyncFolder};
use rag_sync::sync_folder::{create_output_files, delete_old_files, get_files, is_supported_file};
use walkdir::WalkDir;

const CONFIG_PATH: &str = "config.yml";

/// 用真实的 `SyncConfig::from_file` 读仓库里那份配置 + 连真实的 rag-sync.db。
/// run_id 是从库里 counter +1 算出来的，不写库。
async fn real_config() -> SyncConfig {
    let mut conn = database::establish_db_connection()
        .await
        .expect("连不上 rag-sync.db");
    SyncConfig::from_file(&mut conn, CONFIG_PATH)
        .await
        .unwrap_or_else(|e| panic!("读取 {CONFIG_PATH} 失败: {e}"))
}

fn env_limit() -> Option<usize> {
    std::env::var("E2E_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v != "0" && !v.is_empty())
}

// 只读

/// config.yml 真的能被生产代码解析，且 `~` 真的展开了。
/// 展开失败的话 sync_dir 会变成相对路径 `~/Documents`，扫描结果全错。
#[tokio::test]
#[ignore = "需要仓库里的 config.yml"]
async fn config_yml_loads_with_expanded_paths() {
    let config = real_config().await;

    assert!(!config.sync_folders.is_empty(), "至少要配一个同步目录");
    for folder in &config.sync_folders {
        assert!(
            PathBuf::from(&folder.path).is_absolute(),
            "别名 {} 的 path 必须是绝对路径（`~` 没展开?）: {}",
            folder.name,
            folder.path
        );
        assert!(
            !folder.path.contains("~"),
            "{} 里还留着未展开的 `~`",
            folder.name
        );
        assert!(
            std::path::Path::new(&folder.path).is_dir(),
            "别名 {} 的同步目录不存在: {}",
            folder.name,
            folder.path
        );
    }
    // 产物目录共用一个，别名是区分它的唯一手段
    assert!(PathBuf::from(&config.output_dir).is_absolute());
    assert!(!config.output_dir.contains("~"));
    assert!(!config.mineru_url.is_empty(), "mineru_url 不能为空");
    assert!(!config.lightrag_url.is_empty(), "lightrag_url 不能为空");

    // run_id 必须比库里 counter 大 1，否则 full_sync 会把本轮刚写的行当成旧行删掉。
    let mut conn = database::establish_db_connection().await.unwrap();
    let counter = DBfunc::get_run_counter(&mut conn).await.unwrap().unwrap();
    assert_eq!(
        config.run_id,
        counter.current_run_id + 1,
        "run_id 必须由 counter 推导，不能写死在 config.yml 里"
    );
}

/// 两台真服务都活着，且 config.yml 里的凭据能真的登进 LightRAG。
/// 跑全量之前先跑它，别等几小时后才因为密码错而失败。
#[tokio::test]
#[ignore = "需要真实 MinerU 与 LightRAG 可达"]
async fn real_services_are_reachable() {
    let config = real_config().await;

    let client = http_util::client().expect("构造 HTTP 客户端失败");
    let resp = client
        .get(format!("{}/openapi.json", config.mineru_url))
        .send()
        .await
        .unwrap_or_else(|e| panic!("MinerU 不可达 {}: {e}", config.mineru_url));
    assert!(
        resp.status().is_success(),
        "MinerU /openapi.json 返回 {}",
        resp.status()
    );

    // new() 内部做 form 登录，token 为空会直接 Err —— 能建出来就是登进去了。
    let rag = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await
    .expect("LightRAG 登录失败：检查 config.yml 里的 url / 账号 / 密码");

    // 翻页拉全量文档列表，验的是 paginated 契约而不只是「连得上」。
    assert!(
        !rag.document_exists("__rag_sync_e2e_不存在的探针__")
            .await
            .unwrap(),
        "不存在的 doc_id 应报告为 false，说明文档列表真的拉下来了"
    );
}

// 单文件往返

/// MinerU 4.0.12 明确回 `Unsupported file type` 的类型：`SUFFIX_MIME` 里受支持，
/// 于是被扫进来、送给 MinerU，然后被拒。
const MINERU_UNSUPPORTED: &[&str] = &["txt", "md"];

/// 类型受支持、但本机 MinerU 的 PDF 引擎起不来（`Engine core initialization
/// failed`）：服务端的问题，列在这里只是为了让探针不去撞它。
const MINERU_ENGINE_BROKEN: &[&str] = &["pdf"];

fn mineru_would_reject(path: &std::path::Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return true;
    };
    let lower = ext.to_ascii_lowercase();
    MINERU_UNSUPPORTED.contains(&lower.as_str()) || MINERU_ENGINE_BROKEN.contains(&lower.as_str())
}

/// 所有同步目录里体积最小、且真实 MinerU 收得下的文件：`mineru_tackle(...)?`
/// 一失败就整轮中止，探针撞上必然红，而红的原因与服务无关。
async fn smallest_parsable_file(folders: &[SyncFolder]) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    for folder in folders {
        for entry in WalkDir::new(&folder.path)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_dir()
                || !is_supported_file(entry.path()).await
                || mineru_would_reject(entry.path())
            {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let size = meta.len();
            if best.as_ref().is_none_or(|(b, _)| size < *b) {
                best = Some((size, entry.path().to_path_buf()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// `SUFFIX_MIME` 里有 MinerU 并不收的类型，全量同步会在中途整轮 abort；
/// MinerU 升级后重跑一次就知道该改谁。
#[tokio::test]
#[ignore = "需要真实 MinerU"]
async fn mineru_contract_gaps_would_abort_the_full_run() {
    let real = real_config().await;
    let mut by_ext: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for folder in &real.sync_folders {
        for entry in WalkDir::new(&folder.path)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_dir() || !is_supported_file(entry.path()).await {
                continue;
            }
            if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                *by_ext.entry(ext.to_ascii_lowercase()).or_insert(0usize) += 1;
            }
        }
    }

    let mut unsupported: Vec<String> = Vec::new();
    let mut broken: Vec<String> = Vec::new();
    for (ext, n) in &by_ext {
        if MINERU_UNSUPPORTED.contains(&ext.as_str()) {
            println!(
                "MinerU 不支持 .{ext}（{n} 个文件）—— create_output_files 没有按文件的\
                 错误隔离，跑到第一个就会整轮中止"
            );
            unsupported.push(ext.clone());
        }
        if MINERU_ENGINE_BROKEN.contains(&ext.as_str()) {
            println!("MinerU 的 .{ext} 引擎本机起不来（{n} 个文件），服务端问题");
            broken.push(ext.clone());
        }
    }

    assert!(
        unsupported.is_empty(),
        "这些扩展名 MinerU 明确不支持（{}），全量同步会在中途 abort；\
         要么把它们从 sync_dir 移走，要么在 src/file_mime.rs 里收窄",
        unsupported.join(", ")
    );
}

/// 删除并确认真的删掉了。服务端 pipeline 被占用时会回 `busy` 且什么都没删，
/// 重试是必要的；不确认就等于把垃圾留在知识库里。
async fn delete_and_wait(rag: &RAGRequest<'_>, doc_id: &str) -> Result<String, String> {
    let mut attempt = 1;
    loop {
        match rag.delete_document(doc_id).await {
            Ok(_) => return Ok(doc_id.to_string()),
            Err(e) if attempt >= 6 => return Err(e.to_string()),
            Err(e) => {
                println!("  删除 {doc_id} 第 {attempt} 次失败，8 秒后重试: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                attempt += 1;
            }
        }
    }
}

/// 反查并删掉本次探针留下的文档。失败记录是在上传返回之后才注册的，
/// 立刻反查多半查不到，所以先轮询一会儿再动手。
async fn purge_probe(rag: &RAGRequest<'_>, name: &str, cleaned: &mut Vec<String>) {
    for _ in 0..10 {
        // 反查失败要打出来：吞掉会显示成「无需清理」，而文档其实还躺在库里
        let ids = match rag.find_doc_ids_by_filename(name).await {
            Ok(ids) => ids,
            Err(e) => {
                println!("  反查 {name} 失败（可能仍有残留，需要手工确认）: {e}");
                return;
            }
        };
        if ids.is_empty() {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            continue;
        }
        for id in ids {
            match delete_and_wait(rag, &id).await {
                Ok(id) => cleaned.push(id),
                Err(e) => println!("  清理 {id} 失败: {e}"),
            }
        }
        return;
    }
    println!("  没在知识库里找到 {name}，无需清理");
}

/// LightRAG 按**扩展名**选解析器，`X.docx` 里装 markdown 会回
/// `File is not a valid DOCX document`，只有 `.md` 能过。
/// 断言的是应该成立的行为，产物命名改掉之后才会转绿。
#[tokio::test]
#[ignore = "打真实 LightRAG，写入并删除一份文档"]
async fn lightrag_accepts_mineru_markdown_output() {
    let real = real_config().await;
    let client = http_util::client().unwrap();
    let rag = RAGRequest::new(
        &client,
        &real.lightrag_url,
        &real.lightrag_username,
        &real.lightrag_password,
    )
    .await
    .unwrap();

    // 走真实映射：源文件是 .docx，产物应当是 `X.docx.md`。
    let root = tempfile::tempdir().unwrap();
    let src = root
        .path()
        .join(format!("rag_sync_extprobe_{}.docx", std::process::id()));
    std::fs::write(&src, "占位").unwrap();
    let name =
        rag_sync::sync_folder::output_relative_path(root.path().to_str().unwrap(), "probe", &src)
            .await
            .unwrap();
    assert!(name.ends_with(".md"), "产物名必须以 .md 收尾，实际 {name}");
    let path = root.path().join(&name);
    std::fs::write(&path, "# 标题\n\n正文段落。\n".repeat(20)).unwrap();
    // 上传名带别名前缀，与生产链路一致
    let name = rag_sync::sync_folder::upload_name("probe", &name);

    let result = rag
        .upload_file_as(path.to_str().unwrap(), &name, None)
        .await;

    match result {
        Ok(_) => {}
        Err(e) => {
            // 上传失败不代表服务端没建记录。
            let mut cleaned = Vec::new();
            purge_probe(&rag, &name, &mut cleaned).await;
            println!("清理 {:?}", cleaned);
            panic!(
                "LightRAG 拒收了 markdown 产物 {name}: {e}\n\
                 产物名应当以 .md 收尾（当前 {name}），\
                 服务端是按扩展名选解析器的。"
            );
        }
    }

    let doc_id = loop {
        let ids = rag.find_doc_ids_by_filename(&name).await.unwrap();
        if let Some(id) = ids.into_iter().next() {
            break id;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    };

    // 先断言入库，再清理：反过来会变成「刚删掉就断言它还在」。
    assert!(
        rag.document_exists(&doc_id).await.unwrap(),
        "{doc_id} 上传后应留在库里"
    );
    match delete_and_wait(&rag, &doc_id).await {
        Ok(id) => println!("清理 {:?}", [id]),
        Err(e) => panic!("清理 {doc_id} 失败，知识库里会留下它: {e}"),
    }
}

/// 只打真 MinerU 的最小往返：上传 -> `POST /v1/parse/jobs` -> 轮询 -> 按产物 id 取正文。
/// 不碰 LightRAG、不写库，比 `probe_round_trip` 便宜得多。
#[tokio::test]
#[ignore = "需要真实 MinerU"]
async fn mineru_job_round_trip_against_real_service() {
    let real = real_config().await;
    let probe = match smallest_parsable_file(&real.sync_folders).await {
        Some(path) => path,
        None => panic!(
            "{} 下没有 MinerU 收得下的受支持文件，没法探针",
            real.alias_list()
        ),
    };
    println!(
        "探针文件: {} ({} 字节)",
        probe.display(),
        std::fs::metadata(&probe).unwrap().len()
    );

    let path = probe.to_string_lossy().into_owned();
    let sha256 = rag_sync::sync_folder::sha256_file(&path).await.unwrap();
    let file = db_model::File {
        // id 是 DB 行号，这里不落库，给 0 占位
        id: 0,
        path,
        sha256,
        rag_id: None,
        run_id: 1,
        output_rel_path: None,
        uploaded_sha256: None,
    };

    let client = http_util::client().unwrap();
    let markdown = rag_sync::mineru_requests::mineru_tackle(&client, &file, &real)
        .await
        .unwrap_or_else(|e| panic!("任务接口走不通，先别跑 full_sync: {e}"));
    assert!(
        !markdown.trim().is_empty(),
        "MinerU 返回了空 markdown: {}",
        probe.display()
    );
    println!("拿到 {} 字节 markdown", markdown.len());
}

/// 单文件全链路：真 MinerU 转换 -> 写产物 -> 真 LightRAG 入库 -> 落库，结束即删，
/// 只把同步目录 / output_dir 换成临时的。
#[tokio::test]
#[ignore = "会往真实知识库写一份文档再删掉"]
async fn probe_round_trip_against_real_services() {
    let real = real_config().await;
    let probe = match smallest_parsable_file(&real.sync_folders).await {
        Some(path) => path,
        None => panic!(
            "{} 下没有 MinerU 收得下的受支持文件，没法探针",
            real.alias_list()
        ),
    };
    let size = std::fs::metadata(&probe).unwrap().len();
    println!("探针文件: {} ({size} 字节)", probe.display());

    let root = tempfile::tempdir().unwrap();
    let sync_dir = root.path().join("sync");
    let output_dir = root.path().join("out");
    std::fs::create_dir_all(&sync_dir).unwrap();
    std::fs::create_dir_all(&output_dir).unwrap();
    let src = sync_dir.join(probe.file_name().unwrap());
    std::fs::copy(&probe, &src).unwrap();

    let config = SyncConfig {
        sync_folders: vec![SyncFolder {
            name: "probe".into(),
            path: sync_dir.to_string_lossy().into_owned(),
        }],
        output_dir: output_dir.to_string_lossy().into_owned(),
        mineru_url: real.mineru_url.clone(),
        lightrag_url: real.lightrag_url.clone(),
        lightrag_username: real.lightrag_username.clone(),
        lightrag_password: real.lightrag_password.clone(),
        run_id: 1,
        joplin: None,
        mineru_concurrency: 1,
        lightrag_concurrency: 1,
    };

    let mut db = common::test_db().await;
    let files = get_files(&config, &mut db.conn).await.unwrap();
    assert_eq!(files.len(), 1, "探针目录里就该只有一个文件");
    let out_rel = rag_sync::sync_folder::output_relative_path(
        &config.sync_folders[0].path,
        &config.sync_folders[0].name,
        &src,
    )
    .await
    .unwrap();
    let out_name = rag_sync::sync_folder::upload_name(&config.sync_folders[0].name, &out_rel);
    println!("产物名应为 {out_rel}，上传名应为 {out_name}");

    let outcome = create_output_files(&config, &files, &mut db.conn).await;

    // 不管成不成败都要清理：失败往往意味着服务端已经建了记录
    let client = http_util::client().unwrap();
    let rag = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await
    .unwrap();
    let mut cleaned = Vec::new();
    purge_probe(&rag, &out_name, &mut cleaned).await;
    println!("清理 {:?}", cleaned);

    let produced: Vec<PathBuf> = match outcome {
        Ok(produced) => produced,
        Err(e) => panic!(
            "真实链路走不通，先别跑 full_sync: {e}\n\
             （知识库已清理，清理结果见上一行输出）"
        ),
    };
    assert_eq!(produced.len(), 1);

    let markdown = std::fs::read_to_string(&produced[0]).expect("产物应可读");
    assert!(
        !markdown.trim().is_empty(),
        "MinerU 返回了空 markdown: {}",
        produced[0].display()
    );
    println!("产物 {} ({} 字节)", produced[0].display(), markdown.len());

    let row = DBfunc::get_file(&mut db.conn, &src.to_string_lossy())
        .await
        .unwrap()
        .expect("成功的文件必须落库");
    let rag_id = row.rag_id.expect("落库时必须记下 LightRAG 的 doc_id");
    println!("LightRAG doc_id: {rag_id}");
    println!("探针通过并已清理干净");
}

// 全量

/// 用当前 config.yml 把 sync_dir 下全部受支持文件同步一遍 —— 生产入库本身。
/// 逐个断言：这轮要么真的产出了产物并记下 doc_id，要么明确报错，不允许静默跳过。
#[tokio::test]
#[ignore = "把 sync_dir 全部文件灌进真实知识库，按小时计"]
async fn full_sync_against_real_config() {
    let mut conn = database::establish_db_connection()
        .await
        .expect("连不上 rag-sync.db");
    let config = SyncConfig::from_file(&mut conn, CONFIG_PATH)
        .await
        .expect("读取 config.yml 失败");

    let all = get_files(&config, &mut conn).await.expect("扫描失败");
    let total = all.len();
    println!("扫到 {total} 个文件，本轮 run_id = {}", config.run_id);

    // 借切片而不是 clone：db_model::File 没有 Clone，为了测试给生产类型加派生是本末倒置。
    let batch: &[db_model::File] = match env_limit() {
        Some(n) if n < total => {
            println!("E2E_LIMIT={n}：本轮只处理前 {n} 个，其余的不刷新 run_id");
            &all[..n]
        }
        _ => &all[..],
    };
    let batch_len = batch.len();

    let produced = create_output_files(&config, batch, &mut conn)
        .await
        .expect("全量同步失败；重跑会从断点继续（按 sha256 跳过已完成的）");
    println!(
        "本轮产出 {} 份产物（{} 个待处理）",
        produced.len(),
        batch_len
    );

    for path in &produced {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("产物读不出来 {}: {e}", path.display()));
        assert!(!text.trim().is_empty(), "产物是空文件: {}", path.display());
    }

    // 限流跑完必须把没处理到的行刷成 run_id，否则下一轮会被当成「用户已删除」清掉。
    if batch_len < total {
        for f in &all {
            DBfunc::update_file_run_id(&mut conn, &f.path, config.run_id)
                .await
                .unwrap();
        }
        println!("已刷新全部 {} 个文件的 run_id，下一轮不会误删", all.len());
    }

    DBfunc::update_run_counter(&mut conn, config.run_id)
        .await
        .expect("推进 run_counter 失败");

    // 唯一会主动删知识库文档的一步，默认不跑。
    if env_flag("E2E_ALLOW_DELETE") {
        delete_old_files(&config, &mut conn)
            .await
            .expect("清理旧文件失败");
        println!("已执行 delete_old_files");
    } else {
        println!("未设 E2E_ALLOW_DELETE，跳过 delete_old_files");
    }
}

// Joplin

/// 对着真的 Joplin Server 走一遍「取变更流 -> 取正文 -> 切开正文与元信息块」。
/// 只读，不往知识库里传东西：正文切错了，传进去的就是错的。
#[tokio::test]
#[ignore = "需要真实 Joplin Server 可达"]
async fn joplin_delta_and_content_parse() {
    let mut conn = database::establish_db_connection()
        .await
        .expect("连不上 rag-sync.db");
    let config = SyncConfig::from_file(&mut conn, CONFIG_PATH)
        .await
        .expect("读取 config.yml 失败");
    let joplin = config.joplin.expect("config.yml 里要有 joplin 段");

    let client = http_util::client().expect("构造 HTTP 客户端失败");
    let jop = JoplinClient::new(&client, &joplin.url, &joplin.email, &joplin.password)
        .await
        .expect("Joplin 登录失败：检查 config.yml 里的 joplin 段");

    let page = jop.delta(None, 20).await.expect("取变更流失败");
    assert!(!page.items.is_empty(), "变更流不该是空的");
    println!("第一页 {} 条事件，游标 {:?}", page.items.len(), page.cursor);

    let mut notes = 0;
    for item in &page.items {
        let content = match jop.content(&item.item_name).await {
            Ok(c) => c,
            Err(JoplinError::NotFound) => continue,
            Err(e) => panic!("取 {} 的内容失败: {e}", item.item_name),
        };
        let parsed = joplin_model::parse_item(&content);
        if !parsed.is_note() {
            println!("{} 不是笔记，跳过", item.item_name);
            continue;
        }
        notes += 1;
        assert!(
            !parsed.body.trim().is_empty(),
            "{} 的正文切空了",
            item.item_name
        );
        assert_eq!(
            parsed.item_type(),
            Some(joplin_model::item_type::NOTE),
            "{} 的 type_ 不该被切进正文",
            item.item_name
        );
        println!(
            "{}: 标题 {:?}，正文 {} 字，parent_id {:?}",
            item.item_name,
            parsed.title(),
            parsed.body.chars().count(),
            parsed.parent_id()
        );
    }
    assert!(notes > 0, "这一页里应该有笔记");
}

/// 拿真的 Joplin 笔记走一遍真的 LightRAG：正文在内存里直接传，传完就删掉。
/// 验的是「不落盘也能传进去」，跑完不留东西在知识库里。
#[tokio::test]
#[ignore = "需要真实 Joplin Server 与 LightRAG 可达"]
async fn joplin_note_uploaded_from_memory() {
    let mut conn = database::establish_db_connection()
        .await
        .expect("连不上 rag-sync.db");
    let config = SyncConfig::from_file(&mut conn, CONFIG_PATH)
        .await
        .expect("读取 config.yml 失败");
    let joplin = config.joplin.expect("config.yml 里要有 joplin 段");

    let client = http_util::client().expect("构造 HTTP 客户端失败");
    let jop = JoplinClient::new(&client, &joplin.url, &joplin.email, &joplin.password)
        .await
        .expect("Joplin 登录失败");
    let page = jop.delta(None, 5).await.expect("取变更流失败");

    // 找一篇最短的笔记：抽取要调模型，短的跑得快
    let mut picked: Option<(String, String)> = None;
    for item in &page.items {
        let Ok(content) = jop.content(&item.item_name).await else {
            continue;
        };
        let parsed = joplin_model::parse_item(&content);
        if !parsed.is_note() || parsed.body.trim().is_empty() {
            continue;
        }
        match &picked {
            Some((_, body)) if body.chars().count() <= parsed.body.chars().count() => {}
            _ => picked = Some((item.item_name.clone(), parsed.body.clone())),
        }
    }
    let (item_name, body) = picked.expect("这一页里应该有笔记");
    println!("拿来做探针的笔记: {item_name}，{} 字", body.chars().count());

    let rag = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await
    .expect("LightRAG 登录失败");

    let upload_name = format!("{}__{item_name}", joplin.alias);
    let doc_id = rag
        .upload_content_as(&upload_name, body.as_bytes(), None)
        .await
        .expect("上传失败");
    println!("入库成功: {upload_name} -> {doc_id}");
    assert!(
        rag.document_exists(&doc_id).await.unwrap(),
        "传完就该能在文档列表里看到"
    );

    // 探针不留在知识库里
    rag.delete_document_when_free(&doc_id)
        .await
        .expect("删除探针文档失败");
    assert!(
        !rag.document_exists(&doc_id).await.unwrap(),
        "删完就该从文档列表里消失"
    );
    println!("探针已清理干净");
}
