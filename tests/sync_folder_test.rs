//! sha256、相对路径、扫描、以及 delete_old_files。
//!
//! `delete_old_files` 的测试特意分成两段：一段断言它确实在删某个文件，
//! 一段断言它**没有**删它不该删的东西。两段都要有 —— 只写前半段的话，
//! 把同步目录当成输出目录这种错会一路绿灯。

mod common;

use common::{test_db, Env};
use rag_sync::database::DBfunc;
use rag_sync::http_util;
use rag_sync::sync_folder::{delete_old_files, get_files, get_relative_path, sha256_file};
use std::path::Path;

// ---------------------------------------------------------------- sha256

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
    // 循环每次读 8 KiB。构造多个长度都验一遍，能抓到「补零」「少读一字节」
    // 这类只在特定长度出现的错误。
    let d = tempfile::tempdir().unwrap();
    let mut baseline: Option<String> = None;
    for len in [1usize, 8191, 8192, 8193, 16384, 20000] {
        let p = d.path().join(format!("f{len}.bin"));
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &data).unwrap();
        let h = sha256_file(p.to_str().unwrap()).await.unwrap();
        assert_eq!(h.len(), 64, "长度 {len} 的摘要不是 64 个十六进制字符: {h}");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
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

// ---------------------------------------------------------------- 相对路径

#[tokio::test]
async fn relative_path_strips_root() {
    assert_eq!(
        get_relative_path("/data/sync", Path::new("/data/sync/a.pdf")).await.unwrap(),
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
    // get_files 之后按相对路径写输出文件，strip_prefix 失败却当成
    // 「相对路径就是原样」的话，会把输出写到 output_dir 外面去。
    let err = get_relative_path("/data/sync", Path::new("/other/a.pdf"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("/other/a.pdf"), "报错要带上出问题的路径: {err}");
}

#[tokio::test]
async fn relative_path_string_prefix_is_not_root() {
    // "/data/synced" 不是 "/data/sync" 的子路径
    assert!(get_relative_path("/data/sync", Path::new("/data/synced/a.pdf"))
        .await
        .is_err());
}

// ---------------------------------------------------------------- 扫描

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
    // WalkDir 按文件系统顺序返回，不是字典序。处理顺序因此是不确定的，
    // 断言时先排序，别把「顺序不同」误当成 bug。
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
    db.insert_file_row(5, &format!("{}/a.pdf", env.sync_dir), "db-sha", 3, Some("doc-a"))
        .await;
    db.insert_file_row(6, &path, "sha-b", 3, Some("doc-b")).await;

    let files = get_files(&env.config(1), &mut db.conn).await.unwrap();
    let a = files.iter().find(|f| f.path.ends_with("a.pdf")).unwrap();
    assert_eq!(a.id, 5, "库里的身份信息要留用");
    assert_eq!(a.rag_id.as_deref(), Some("doc-a"));
    assert_eq!(a.run_id, 3, "run_id 原样带出，create_output_files 据此判断是否刷新");
    assert_eq!(
        a.sha256,
        sha256_file(&format!("{}/a.pdf", env.sync_dir)).await.unwrap(),
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
        assert!(
            err.contains("锁住"),
            "要指出是哪个目录读不了: {err}"
        );
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

/// HTTP 客户端必须带超时。reqwest 默认既不限制连接也不限制读取，
/// 而轮询里的「上限」只在请求**返回之后**才检查得到 ——
/// 也就是说服务端不回包的话，上限完全失效，整个进程会永久挂住。
///
/// 用 tokio 的自动推进时钟：READ_TIMEOUT 是 60 s 真实时间，
/// 这里不会真的等 60 s。
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

    let err = result.err().expect("服务端不应回包，请求必须失败");
    assert!(
        err.is_timeout() || format!("{err}").contains("timed out"),
        "期望是超时错误，实际: {err}"
    );
    assert!(
        waited >= http_util::READ_TIMEOUT,
        "应当一直等到 READ_TIMEOUT 才放弃，实际只等了 {waited:?}"
    );
}

#[tokio::test]
async fn undeletable_artifact_keeps_db_row() {
    // 「产物删不掉」和「产物不存在」必须区别对待：当成不存在就等于
    // 跳过删除、接着把 DB 行删掉，产物从此永久失去关联再也找不到。
    // 这里在产物路径上放一个**目录**来触发删除失败，无需依赖权限位。
    let mut db = test_db().await;
    let env = Env::new();
    let src = env.write_source("a.pdf", "原件");
    std::fs::create_dir_all(env.output_path("a.pdf")).unwrap();
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
    assert!(!err.contains(&good), "成功的那个不该出现在失败列表里: {err}");

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

#[tokio::test]
async fn scan_empty_dir_returns_empty() {
    let mut db = test_db().await;
    let env = Env::new();
    assert!(get_files(&env.config(1), &mut db.conn).await.unwrap().is_empty());
}

// ---------------------------------------------------------------- delete_old_files

#[tokio::test]
async fn delete_old_returns_early_when_nothing_is_old() {
    // Env 的 lightrag_url 是空串，登录必然失败。这里能过，说明清理动作
    // 在「没有旧文件」时确实一步都没做 —— 服务不可达不该拖垮一轮同步。
    let mut db = test_db().await;
    let env = Env::new();
    delete_old_files(&env.config(4), &mut db.conn).await.unwrap();
}

#[tokio::test]
async fn cleanup_tolerates_missing_artifacts() {
    let mut db = test_db().await;
    let env = Env::new();
    let cur = env.write_source("a.pdf", "x");
    db.insert_file_row(1, "/nowhere/gone.pdf", "s1", 1, None).await; // 路径不在 sync_dir 下
    db.insert_file_row(2, &cur, "s2", 1, None).await; // 路径在，但没有产物文件

    let server = common::MockServer::start(vec![
        common::Route::post("/login")
            .and(common::CannedResponse::json(200, r#"{"access_token":"t"}"#)),
    ])
    .await;
    let mut cfg = env.config(5);
    cfg.lightrag_url = server.base_url.clone();

    delete_old_files(&cfg, &mut db.conn).await.unwrap();

    assert!(!DBfunc::file_exists(&mut db.conn, "/nowhere/gone.pdf").await.unwrap());
    assert!(!DBfunc::file_exists(&mut db.conn, &cur).await.unwrap());
    assert_eq!(
        server.count_to("DELETE", "/documents/delete_document"),
        0,
        "没有 rag_id 就不该去删知识库文档"
    );
}
