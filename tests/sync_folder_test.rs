//! sha256、相对路径、扫描、delete_old_files。
//! 后者分两段断言：确实在删，且没有删不该删的。

mod common;

use common::{Env, test_db};
use rag_sync::database::DBfunc;
use rag_sync::http_util;
use rag_sync::sync_folder::{
    delete_old_files, get_files, get_relative_path, output_relative_path, sha256_file, upload_name,
};
use std::path::Path;

// sha256

#[tokio::test]
async fn sha256_matches_known_vectors() {
    let d = tempfile::tempdir().unwrap();
    let empty = d.path().join("empty.bin");
    std::fs::write(&empty, b"").unwrap();
    assert_eq!(
        sha256_file(empty.to_str().unwrap()).await.unwrap(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );

    let abc = d.path().join("abc.txt");
    std::fs::write(&abc, b"abc").unwrap();
    assert_eq!(
        sha256_file(abc.to_str().unwrap()).await.unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[tokio::test]
async fn sha256_handles_buffer_boundaries() {
    // 循环每次读 8 KiB：多个长度都验一遍，能抓到「补零」这类只在特定长度出现的错误
    let d = tempfile::tempdir().unwrap();
    let mut baseline: Option<String> = None;
    for len in [1usize, 8191, 8192, 8193, 16384, 20000] {
        let p = d.path().join(format!("f{len}.bin"));
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        let h = sha256_file(p.to_str().unwrap()).await.unwrap();
        assert_eq!(h.len(), 64, "长度 {len} 的摘要不是 64 个十六进制字符: {h}");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        match &baseline {
            None => baseline = Some(h),
            Some(b) => assert_ne!(b, &h, "不同内容（{len} 字节）得到了同一个摘要"),
        }
    }
}

#[tokio::test]
async fn sha256_differs_on_single_byte() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a.bin");
    let b = d.path().join("b.bin");
    let base = vec![b'x'; 10_000];
    let mut tweaked = base.clone();
    tweaked[9_999] = b'y';
    std::fs::write(&a, &base).unwrap();
    std::fs::write(&b, &tweaked).unwrap();
    assert_ne!(
        sha256_file(a.to_str().unwrap()).await.unwrap(),
        sha256_file(b.to_str().unwrap()).await.unwrap()
    );
}

#[tokio::test]
async fn sha256_missing_file_is_an_error() {
    let err = sha256_file("/no/such/file.pdf").await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

// 相对路径

#[tokio::test]
async fn relative_path_strips_root() {
    assert_eq!(
        get_relative_path("/data/sync", Path::new("/data/sync/a.pdf"))
            .await
            .unwrap(),
        "a.pdf"
    );
    assert_eq!(
        get_relative_path("/data/sync", Path::new("/data/sync/sub/dir/a.pdf"))
            .await
            .unwrap(),
        "sub/dir/a.pdf"
    );
}

#[tokio::test]
async fn relative_path_outside_root_errors() {
    // strip_prefix 失败却当成「原样」的话，输出会写到 output_dir 外面去
    let err = get_relative_path("/data/sync", Path::new("/other/a.pdf"))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("/other/a.pdf"),
        "报错要带上出问题的路径: {err}"
    );
}

#[tokio::test]
async fn relative_path_string_prefix_is_not_root() {
    // "/data/synced" 不是 "/data/sync" 的子路径
    assert!(
        get_relative_path("/data/sync", Path::new("/data/synced/a.pdf"))
            .await
            .is_err()
    );
}

// 扫描

#[tokio::test]
async fn scan_only_picks_supported_suffixes_recursively() {
    let mut db = test_db().await;
    let env = Env::new();
    env.write_source("a.pdf", "pdf-a");
    env.write_source("b.docx", "docx-b");
    env.write_source("nested/c.txt", "txt-c");
    env.write_source("skip.png", "not-a-doc");
    env.write_source("skip.zip", "not-a-doc");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let mut names: Vec<String> = files
        .iter()
        .map(|f| f.path.rsplit('/').next().unwrap().to_string())
        .collect();
    // WalkDir 按文件系统顺序返回，断言时先排序，别把「顺序不同」误当成 bug。
    names.sort();
    assert_eq!(names, vec!["a.pdf", "b.docx", "c.txt"]);
}

#[tokio::test]
async fn scan_hashes_new_files() {
    let mut db = test_db().await;
    let env = Env::new();
    let path = env.write_source("a.pdf", "hello");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].id, 0, "id=0 是 NOT_IN_DB 标记");
    assert_eq!(files[0].path, path);
    assert_eq!(files[0].rag_id, None);
    assert_eq!(
        files[0].sha256,
        sha256_file(&path).await.unwrap(),
        "新增文件的 sha256 必须现算，不能留空"
    );
}

#[tokio::test]
async fn scan_keeps_db_identity_but_refreshes_sha() {
    let mut db = test_db().await;
    let env = Env::new();
    env.write_source("a.pdf", "hello");
    let path = env.write_source("b.pdf", "world");
    // a.pdf 在库里，且**故意存一个和磁盘内容不符的 sha256**
    db.insert_file_row(
        5,
        &format!("{}/a.pdf", env.sync_dir),
        "db-sha",
        3,
        Some("doc-a"),
    )
    .await;
    db.insert_file_row(6, &path, "sha-b", 3, Some("doc-b"))
        .await;

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let a = files.iter().find(|f| f.path.ends_with("a.pdf")).unwrap();
    assert_eq!(a.id, 5, "库里的身份信息要留用");
    assert_eq!(a.rag_id.as_deref(), Some("doc-a"));
    assert_eq!(
        a.run_id, 3,
        "run_id 原样带出，create_output_files 据此判断是否刷新"
    );
    assert_eq!(
        a.sha256,
        sha256_file(&format!("{}/a.pdf", env.sync_dir))
            .await
            .unwrap(),
        "sha256 必须取磁盘实况，否则文件改了永远看不出来"
    );
    assert_ne!(a.sha256, "db-sha");

    let b = files.iter().find(|f| f.path.ends_with("b.pdf")).unwrap();
    assert_eq!(b.sha256, sha256_file(&path).await.unwrap());
}

#[tokio::test]
async fn scan_returns_sorted_paths() {
    let mut db = test_db().await;
    let env = Env::new();
    for name in ["c.pdf", "a.pdf", "b.pdf"] {
        env.write_source(name, name);
    }
    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let names: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "处理顺序要可复现，不能跟着文件系统顺序走");
}

// 多文件夹扫描

/// 每个别名都要被扫到：漏扫的目录会被 delete_old_files 当成「用户删了」清掉。
#[tokio::test]
async fn scan_covers_every_alias() {
    let mut db = test_db().await;
    let mut env = Env::new();
    env.write_source("工作/a.pdf", "x");
    let personal = env.add_folder("personal");
    let b = env.write_source_in("personal", "b.pdf", "y");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(
        paths.iter().any(|p| p.starts_with(&env.sync_dir)),
        "{paths:?}"
    );
    assert!(paths.iter().any(|p| p.starts_with(&personal)), "{paths:?}");
    assert!(paths.contains(&b.as_str()), "{paths:?}");
}

/// 处理顺序要跨别名整体可复现，不能一个目录一段。
#[tokio::test]
async fn scan_sorts_across_aliases() {
    let mut db = test_db().await;
    let mut env = Env::new();
    env.write_source("z.pdf", "z");
    env.add_folder("personal");
    env.write_source_in("personal", "a.pdf", "a");

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);
}

/// 中途失败必须整轮中止，报错要指出是哪个别名下的哪个目录。
#[tokio::test]
async fn unreadable_folder_aborts_the_whole_run() {
    let mut db = test_db().await;
    let mut env = Env::new();
    env.write_source("能读/a.pdf", "x");
    let personal = env.add_folder("personal");
    let locked = Path::new(&personal).join("锁住");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("b.pdf"), "x").unwrap();
    let mut perms = std::fs::metadata(&locked).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o000);
    }
    std::fs::set_permissions(&locked, perms).unwrap();

    let files = get_files(&env.config(1), &mut db.conn).await;

    #[cfg(unix)]
    {
        // root 无视权限位，那样就造不出这个错误，跳过而不是假装通过
        if std::fs::read_dir(&locked).is_ok() {
            eprintln!("以 root 运行，权限位无效，跳过本用例");
            let _ = std::fs::remove_dir_all(&locked);
            return;
        }
        let err = files.unwrap_err().to_string();
        assert!(err.contains("personal"), "要指出是哪个别名扫不了: {err}");
        assert!(err.contains("锁住"), "要指出是哪个目录读不了: {err}");
        assert!(err.contains("中止"), "跳过的别名会被当成已删除: {err}");
        let _ = std::fs::remove_dir_all(&locked);
    }
    #[cfg(not(unix))]
    {
        let _ = files;
    }
}

#[tokio::test]
async fn unreadable_subdirectory_aborts_the_run() {
    let mut db = test_db().await;
    let env = Env::new();
    env.write_source("能读/a.pdf", "x");
    let locked = Path::new(&env.sync_dir).join("锁住");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("b.pdf"), "x").unwrap();
    // 去掉读权限，制造一个「子目录存在但读不了」的错误
    let mut perms = std::fs::metadata(&locked).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o000);
    }
    std::fs::set_permissions(&locked, perms).unwrap();

    let cfg = env.config(1);
    let files = get_files(&cfg, &mut db.conn).await;

    #[cfg(unix)]
    {
        // root 无视权限位，那样就造不出这个错误，跳过而不是假装通过
        if std::fs::read_dir(&locked).is_ok() {
            eprintln!("以 root 运行，权限位无效，跳过本用例");
            let _ = std::fs::remove_dir_all(&locked);
            return;
        }
        let err = files.unwrap_err().to_string();
        assert!(err.contains("锁住"), "要指出是哪个目录读不了: {err}");
        assert!(
            err.contains("中止"),
            "跳过的目录会被 delete_old_files 当成「用户删了」而清掉产物与知识库，必须中止: {err}"
        );
        let _ = std::fs::remove_dir_all(&locked);
    }
    #[cfg(not(unix))]
    {
        let _ = files;
    }
}

/// HTTP 客户端必须带超时，否则服务端不回包就会永久挂住。
/// 用 tokio 的自动推进时钟，这里不会真的等 60 s。
#[tokio::test(start_paused = true)]
async fn http_client_times_out_on_silent_server() {
    // 只 accept 不回应：请求永远拿不到响应
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            held.push(s);
        }
    });

    let client = http_util::client().unwrap();
    let url = format!("http://{addr}/v1/uploads");

    let started = tokio::time::Instant::now();
    let result = client.get(&url).send().await;
    let waited = started.elapsed();

    let err = result.expect_err("服务端不应回包，请求必须失败");
    assert!(
        err.is_timeout() || format!("{err}").contains("timed out"),
        "期望是超时错误，实际: {err}"
    );
    assert!(
        waited >= http_util::READ_TIMEOUT,
        "应当一直等到 READ_TIMEOUT 才放弃，实际只等了 {waited:?}"
    );
}

// 产物命名：必须以 .md 收尾，但直接改成 X.md 会让同名主干的文件互相覆盖，
// 保留原扩展名作中间段才是无损映射；多个同步目录共用一个 output_dir，第一段是别名。

#[tokio::test]
async fn output_relative_path_switches_to_markdown() {
    assert_eq!(
        output_relative_path("/root", "work", Path::new("/root/a.pdf"))
            .await
            .unwrap(),
        "work/a.pdf.md"
    );
}

#[tokio::test]
async fn output_relative_path_keeps_nested_dirs() {
    assert_eq!(
        output_relative_path("/root", "work", Path::new("/root/2024/一季度/a.docx"))
            .await
            .unwrap(),
        "work/2024/一季度/a.docx.md"
    );
}

/// 别名就是跨目录区分产物的手段：目录结构、文件名都相同的两个源文件，
/// 产物必须落在不同的子目录下。
#[tokio::test]
async fn output_relative_path_separates_aliases() {
    let a = output_relative_path("/a", "work", Path::new("/a/2024/报告.pdf"))
        .await
        .unwrap();
    let b = output_relative_path("/b", "personal", Path::new("/b/2024/报告.pdf"))
        .await
        .unwrap();
    assert_ne!(a, b, "两个别名的同名文件不能产出同一个路径");
    assert_eq!(a, "work/2024/报告.pdf.md");
    assert_eq!(b, "personal/2024/报告.pdf.md");
}

/// 同名主干（`竞选.pdf` / `竞选.pptx`）不能映射到同一个产物名，否则后写的覆盖先写的。
#[tokio::test]
async fn output_relative_path_avoids_stem_collisions() {
    let from_pdf = output_relative_path("/root", "work", Path::new("/root/竞选/竞选.pdf"))
        .await
        .unwrap();
    let from_pptx = output_relative_path("/root", "work", Path::new("/root/竞选/竞选.pptx"))
        .await
        .unwrap();
    assert_ne!(from_pdf, from_pptx, "同名主干必须映射到不同产物名");
    assert!(from_pdf.ends_with(".md") && from_pptx.ends_with(".md"));
}

/// 源文件本来就是 `.md` 的不加中间段，否则 `X.md` 会和 `X.md.md` 撞在一起。
#[tokio::test]
async fn output_relative_path_does_not_double_markdown_suffix() {
    assert_eq!(
        output_relative_path("/root", "work", Path::new("/root/发言稿.md"))
            .await
            .unwrap(),
        "work/发言稿.md"
    );
}

/// 映射只能依赖源路径本身：cleanup_one_old_file 手上只有一条 DB 记录，也得算出同一个产物名。
#[tokio::test]
async fn output_relative_path_is_stable_across_calls() {
    let first = output_relative_path("/root", "work", Path::new("/root/x/a.docx"))
        .await
        .unwrap();
    let second = output_relative_path("/root", "work", Path::new("/root/x/a.docx"))
        .await
        .unwrap();
    assert_eq!(first, second);
}

// 上传名：LightRAG 的文档唯一键是文件名，产物所在的子目录它看不见，
// 别名前缀是唯一能防住跨目录同名互删的东西。

#[test]
fn upload_name_carries_the_alias() {
    assert_eq!(
        upload_name("work", "work/2024/报告.pdf.md"),
        "work__报告.pdf.md"
    );
}

#[test]
fn upload_name_distinguishes_same_name_in_two_aliases() {
    let work = upload_name("work", "work/报告.pdf.md");
    let personal = upload_name("personal", "personal/报告.pdf.md");
    assert_ne!(work, personal, "上传名撞了就会 409 自愈，先传的那份被删");
}

/// 别名之外的字符一律原样保留：上传名要能让人认出是哪个文件。
#[test]
fn upload_name_keeps_the_whole_artifact_name() {
    assert_eq!(
        upload_name("work", "work/2024/一季度/a.docx.md"),
        "work__a.docx.md"
    );
    assert_eq!(upload_name("work", "work/发言稿.md"), "work__发言稿.md");
}

// 清理

#[tokio::test]
async fn undeletable_artifact_keeps_db_row() {
    // 「产物删不掉」和「产物不存在」必须区别对待：当成不存在就等于删掉 DB 行，
    // 产物从此永久失去关联。这里在产物路径上放一个目录来触发删除失败。
    let mut db = test_db().await;
    let env = Env::new();
    let src = env.write_source("a.pdf", "原件");
    // 放在产物真正所在的位置：产物名换过扩展名，写 a.pdf 的话 remove_file 会拿到
    // NotFound 而静默跳过，测试假绿。
    std::fs::create_dir_all(env.output_path("a.pdf.md")).unwrap();
    db.insert_file_row(1, &src, "sha", 1, Some("doc-1")).await;

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
        common::Route::delete("/documents/delete_document").and(
            common::CannedResponse::json(
                200,
                r#"{"status":"deletion_started","message":"ok","doc_id":"doc-1"}"#,
            ),
        ),
        common::Route::post("/documents/paginated")
            .and(common::CannedResponse::json(
                200,
                r#"{"documents":[{"id":"doc-1","status":"processed","file_path":"a.pdf"}],
                     "pagination":{"page":1,"total_pages":1,"total_count":1,"has_next":false}}"#,
            ))
            .and(common::CannedResponse::json(
                200,
                r#"{"documents":[],"pagination":{"page":1,"total_pages":1,"total_count":0,"has_next":false}}"#,
            )),
    ])
    .await;
    let mut cfg = env.config(2);
    cfg.lightrag_url = server.base_url.clone();

    let err = delete_old_files(&cfg, &mut db.conn)
        .await
        .unwrap_err()
        .to_string();

    // 知识库那步成功了，卡在产物删除
    assert_eq!(
        server.requests_to("DELETE", "/documents/delete_document")[0].json()["doc_ids"][0],
        "doc-1"
    );
    assert!(err.contains("删除输出文件"), "{err}");
    assert!(
        DBfunc::file_exists(&mut db.conn, &src).await.unwrap(),
        "产物没删掉就不能删 DB 行，否则下轮再也不会来清这个产物"
    );
    assert!(std::path::Path::new(&src).exists(), "原件始终不能动");
}

#[tokio::test]
async fn one_cleanup_failure_does_not_block_others() {
    let mut db = test_db().await;
    let env = Env::new();
    let bad = env.write_source("bad.md", "x");
    let good = env.write_source("good.md", "x");
    // bad 的产物是个目录，删不掉
    std::fs::create_dir_all(env.output_path("bad.md")).unwrap();
    std::fs::write(env.output_path("good.md"), "# 过期").unwrap();
    db.insert_file_row(1, &bad, "s1", 1, None).await;
    db.insert_file_row(2, &good, "s2", 1, None).await;

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(2);
    cfg.lightrag_url = server.base_url.clone();

    let err = delete_old_files(&cfg, &mut db.conn)
        .await
        .unwrap_err()
        .to_string();

    // 整体仍然失败（让本轮以非零码退出，计数器不推进，下轮重试）
    assert!(err.contains("清理 1 个旧文件失败"), "{err}");
    assert!(err.contains(&bad), "汇总里要指明是哪个文件: {err}");
    assert!(
        !err.contains(&good),
        "成功的那个不该出现在失败列表里: {err}"
    );

    assert!(
        DBfunc::file_exists(&mut db.conn, &bad).await.unwrap(),
        "失败的那个要留下 DB 行等下轮重试"
    );
    assert!(
        !DBfunc::file_exists(&mut db.conn, &good).await.unwrap(),
        "成功的那个照样要清掉"
    );
    assert!(
        !env.output_path("good.md").exists(),
        "成功那条的产物也要清掉"
    );
}

/// 产物路径按 DB 记录删，不按配置现算：改了别名之后旧产物仍在原位置，
/// 现算会指向一个从没写过的路径，删不到就永远留在 output_dir 里。
#[tokio::test]
async fn cleanup_uses_the_recorded_artifact_path() {
    let mut db = test_db().await;
    let env = Env::new();
    let src = env.write_source("a.pdf", "x");
    // 产物直接落在 output_dir 下（库里记的就是这个），不经过 output_path ——
    // 后者会拼上当前别名。
    let artifact = Path::new(&env.output_dir).join("work/a.pdf.md");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, "# 旧别名下的产物").unwrap();
    db.insert_file_row_full(1, &src, "s1", 1, None, Some("work/a.pdf.md"))
        .await;

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(2);
    cfg.lightrag_url = server.base_url.clone();

    delete_old_files(&cfg, &mut db.conn).await.unwrap();

    assert!(
        !artifact.exists(),
        "库里记的是 work/a.pdf.md，现在算出来的是 sync/a.pdf.md —— 认错位置就删不掉"
    );
}

/// 加列之前入库的行没有 output_rel_path：两个候选都试，真正那个（旧规则、
/// 不带别名目录）也要能删掉。「算不出带别名的路径」不能被当成「找不到」。
#[tokio::test]
async fn cleanup_falls_back_to_the_legacy_artifact_path() {
    let mut db = test_db().await;
    let env = Env::new();
    let src = env.write_source("旧报告.pdf", "x");
    // 旧布局：产物直接躺在 output_dir 下，没有别名那一层；两个候选都试才删得到
    let artifact = Path::new(&env.output_dir).join("旧报告.pdf.md");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, "# 旧布局产物").unwrap();
    db.insert_file_row_full(1, &src, "s1", 1, None, None).await;

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(2);
    cfg.lightrag_url = server.base_url.clone();

    delete_old_files(&cfg, &mut db.conn).await.unwrap();

    assert!(
        !artifact.exists(),
        "旧布局的产物也要删得到，否则它永远失去关联"
    );
    assert!(!DBfunc::file_exists(&mut db.conn, &src).await.unwrap());
}

/// 加列前入库、源文件又已经移出配置目录的行：反推不出产物位置，
/// 但 DB 行必须清掉，否则每一轮都会重来一遍并卡住清理。
#[tokio::test]
async fn cleanup_removes_row_even_when_artifact_is_unfindable() {
    let mut db = test_db().await;
    let env = Env::new();
    let old_src = "/home/old/Documents/旧报告.pdf";
    db.insert_file_row_full(1, old_src, "s1", 1, None, None)
        .await;

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(2);
    cfg.lightrag_url = server.base_url.clone();

    delete_old_files(&cfg, &mut db.conn).await.unwrap();

    assert!(
        !DBfunc::file_exists(&mut db.conn, old_src).await.unwrap(),
        "定位不到产物不能让这一行把清理永久卡住"
    );
}

/// 产物路径记错了也要真去删那一行，不能当成「不在了」就把 DB 行删掉。
#[tokio::test]
async fn cleanup_prefers_recorded_path_even_if_file_is_gone() {
    let mut db = test_db().await;
    let env = Env::new();
    let src = env.write_source("a.pdf", "x");
    // 记了一个不存在的路径：模拟产物被人手工挪走
    db.insert_file_row_full(1, &src, "s1", 1, None, Some("work/别处/a.pdf.md"))
        .await;

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(2);
    cfg.lightrag_url = server.base_url.clone();

    // 文件不在了正是预期结果，不该报错
    delete_old_files(&cfg, &mut db.conn).await.unwrap();
    assert!(!DBfunc::file_exists(&mut db.conn, &src).await.unwrap());
}

#[tokio::test]
async fn scan_empty_dir_returns_empty() {
    let mut db = test_db().await;
    let env = Env::new();
    assert!(
        get_files(&env.config(1), &mut db.conn)
            .await
            .unwrap()
            .is_empty()
    );
}

/// 清理时要按别名子目录去找产物：算错一层就删不到文件。
#[tokio::test]
async fn cleanup_deletes_artifact_of_the_right_alias() {
    let mut db = test_db().await;
    let mut env = Env::new();
    let personal = env.add_folder("personal");
    let src = env.write_source_in("personal", "报告.pdf", "x");
    let artifact = env.output_path_in("personal", "报告.pdf.md");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, "# 过期").unwrap();
    db.insert_file_row(1, &src, "s1", 1, None).await;

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(2);
    cfg.lightrag_url = server.base_url.clone();

    delete_old_files(&cfg, &mut db.conn).await.unwrap();

    assert!(!artifact.exists(), "产物应按 <别名>/ 删掉");
    assert!(!DBfunc::file_exists(&mut db.conn, &src).await.unwrap());
    assert!(Path::new(&personal).is_dir(), "只删产物，不碰同步目录本身");
    assert!(Path::new(&src).exists(), "原件始终不能动");
}

/// 别名从配置里删掉后，产物照样删得到 —— 靠的是库里记的路径，不是配置。
/// 这正是加这一列的意义。
#[tokio::test]
async fn cleanup_finds_artifact_even_after_alias_is_removed() {
    let mut db = test_db().await;
    let env = Env::new();
    let src = env.write_source("a.pdf", "x");
    // 产物位置按库里记的来，所以直接写在 output_dir 根下，不拼别名
    let artifact = Path::new(&env.output_dir).join("a.pdf.md");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, "# 过期").unwrap();
    db.insert_file_row_full(1, &src, "s1", 1, None, Some("a.pdf.md"))
        .await;

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(2);
    cfg.lightrag_url = server.base_url.clone();
    // 换一个不包含该文件的目录，等价于用户改了配置
    cfg.sync_folders = vec![rag_sync::sync_config::SyncFolder {
        name: "elsewhere".into(),
        path: env
            .root
            .path()
            .join("elsewhere")
            .to_string_lossy()
            .into_owned(),
    }];

    delete_old_files(&cfg, &mut db.conn).await.unwrap();

    assert!(
        !artifact.exists(),
        "别名没了但库里记着路径，产物必须删掉，否则它永远留在 output_dir 里"
    );
    assert!(!DBfunc::file_exists(&mut db.conn, &src).await.unwrap());
}

// delete_old_files

#[tokio::test]
async fn delete_old_returns_early_when_nothing_is_old() {
    // Env 的 lightrag_url 是空串，登录必然失败。这里能过说明「没有旧文件」时一步都没做
    let mut db = test_db().await;
    let env = Env::new();
    delete_old_files(&env.config(4), &mut db.conn)
        .await
        .unwrap();
}

#[tokio::test]
async fn cleanup_tolerates_missing_artifacts() {
    let mut db = test_db().await;
    let env = Env::new();
    let cur = env.write_source("a.pdf", "x");
    db.insert_file_row(1, "/nowhere/gone.pdf", "s1", 1, None)
        .await; // 路径不在 sync_dir 下
    db.insert_file_row(2, &cur, "s2", 1, None).await; // 路径在，但没有产物文件

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(5);
    cfg.lightrag_url = server.base_url.clone();

    delete_old_files(&cfg, &mut db.conn).await.unwrap();

    assert!(
        !DBfunc::file_exists(&mut db.conn, "/nowhere/gone.pdf")
            .await
            .unwrap()
    );
    assert!(!DBfunc::file_exists(&mut db.conn, &cur).await.unwrap());
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        0,
        "没有 rag_id 就不该去删知识库文档"
    );
}
