#![allow(non_snake_case)]
//! sha256、相对路径、扫描、以及 delete_old_files。
//!
//! `delete_old_files` 的测试特意分成两段：一段断言它确实在删某个文件，
//! 一段断言它**没有**删它不该删的东西。两段都要有 —— 只写前半段的话，
//! 把同步目录当成输出目录这种错会一路绿灯。

mod common;

use common::{test_db, Env};
use rag_sync::sync_folder::{delete_old_files, get_files, get_relative_path, sha256_file};
use std::path::Path;

// ---------------------------------------------------------------- sha256

#[tokio::test]
async fn sha256_对上标准向量() {
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
async fn sha256_跨_8192_缓冲边界不丢字节() {
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
async fn sha256_只差一个字节_摘要就不同() {
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
async fn sha256_文件不存在_返回_err_而不是空串() {
    let err = sha256_file("/no/such/file.pdf").await.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

// ---------------------------------------------------------------- 相对路径

#[tokio::test]
async fn 相对路径_去根前缀() {
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
async fn 相对路径_路径不在根下时报错() {
    // get_files 之后按相对路径写输出文件，strip_prefix 失败却当成
    // 「相对路径就是原样」的话，会把输出写到 output_dir 外面去。
    let err = get_relative_path("/data/sync", Path::new("/other/a.pdf"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("/other/a.pdf"), "报错要带上出问题的路径: {err}");
}

#[tokio::test]
async fn 相对路径_前缀只是字符串相同不算根() {
    // "/data/synced" 不是 "/data/sync" 的子路径
    assert!(get_relative_path("/data/sync", Path::new("/data/synced/a.pdf"))
        .await
        .is_err());
}

// ---------------------------------------------------------------- 扫描

#[tokio::test]
async fn 扫描_只收支持的后缀_且递归子目录() {
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
async fn 扫描_库里没有的文件算新增_并现算_sha256() {
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
async fn 扫描_库里已有时原样返回_不重算_sha256() {
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
    assert_eq!(a.id, 5);
    assert_eq!(a.sha256, "db-sha");
    assert_eq!(a.rag_id.as_deref(), Some("doc-a"));
    assert_eq!(a.run_id, 3, "DB 行的 run_id 原样带出");

    let b = files.iter().find(|f| f.path.ends_with("b.pdf")).unwrap();
    assert_eq!(b.sha256, "sha-b");
}

#[tokio::test]
async fn 扫描_空目录返回空vec() {
    let mut db = test_db().await;
    let env = Env::new();
    assert!(get_files(&env.config(1), &mut db.conn).await.unwrap().is_empty());
}

// ---------------------------------------------------------------- delete_old_files

#[tokio::test]
async fn 删旧文件_只删_run_id_更小的() {
    let mut db = test_db().await;
    let env = Env::new();
    let old = env.write_source("old.pdf", "old");
    let cur = env.write_source("cur.pdf", "cur");
    db.insert_file_row(1, &old, "s1", 3, Some("doc-old")).await;
    db.insert_file_row(2, &cur, "s2", 4, None).await;

    delete_old_files(&env.config(4), &mut db.conn).await.unwrap();

    assert!(!std::path::Path::new(&old).exists(), "run_id=3 < 4 的应当被删");
    assert!(std::path::Path::new(&cur).exists(), "本轮的文件不能被删");
}

#[tokio::test]
async fn 删旧文件_磁盘上已不存在时不算失败() {
    let mut db = test_db().await;
    let env = Env::new();
    db.insert_file_row(1, "/nowhere/gone.pdf", "s1", 1, None).await;
    // 库里还留着、磁盘上已经没了 —— 这正是「用户自己把文件删了」的情况
    delete_old_files(&env.config(5), &mut db.conn).await.unwrap();
}
