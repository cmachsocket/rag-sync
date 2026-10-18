#![allow(non_snake_case)]
//! 配置加载与 run_id 的推导。
//!
//! run_id 是整个同步流程的时钟：它既决定「哪些文件算这轮新增」，
//! 又决定 `delete_old_files` 会删谁。推错一位就是误删，所以单独钉死。

mod common;

use common::test_db;

fn 写配置(dir: &tempfile::TempDir, body: &str) -> String {
    let p = dir.path().join("config.yml");
    std::fs::write(&p, body).unwrap();
    p.to_string_lossy().into_owned()
}

#[tokio::test]
async fn 读全字段() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(
        &d,
        r#"
sync_dir: "/data/sync"
output_dir: "/data/out"
mineru_url: "http://m:9777"
lightrag_url: "http://r:9621"
lightrag_username: "u"
lightrag_password: "p"
"#,
    );
    let c = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap();
    assert_eq!(c.sync_dir, "/data/sync");
    assert_eq!(c.output_dir, "/data/out");
    assert_eq!(c.mineru_url, "http://m:9777");
    assert_eq!(c.lightrag_url, "http://r:9621");
    assert_eq!(c.lightrag_username, "u");
    assert_eq!(c.lightrag_password, "p");
}

#[tokio::test]
async fn 缺省键取默认值_不影响加载() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(
        &d,
        r#"
sync_dir: "/data/sync"
output_dir: "/data/out"
mineru_url: "http://m:9777"
lightrag_url: "http://r:9621"
"#,
    );
    let c = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap();
    assert_eq!(c.lightrag_username, "");
    assert_eq!(c.lightrag_password, "");
    // run_id 在解析后被计数器 +1 无条件覆盖，serde 上的 `default = "zero"`
    // 实际不可达（配置里写什么都不影响，见下面「配置里的_run_id_不参与计算」）
    assert_eq!(c.run_id, 1);
}

#[tokio::test]
async fn 缺_sync_dir_报错并指出字段名() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(&d, "output_dir: \"/data/out\"\nmineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n");
    let err = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("sync_dir"), "报错要说清缺哪个字段: {err}");
}

#[tokio::test]
async fn 缺_output_dir_报错并指出字段名() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(&d, "sync_dir: \"/data/sync\"\nmineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n");
    let err = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("output_dir"), "报错要说清缺哪个字段: {err}");
}

#[tokio::test]
async fn 空字符串等同于缺失() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(&d, "sync_dir: \"\"\noutput_dir: \"/data/out\"\nmineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n");
    let err = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("sync_dir"), "空串应被当成缺失: {err}");
}

#[tokio::test]
async fn 缺_mineru_url_报错并指出字段名() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(
        &d,
        "sync_dir: \"/a\"\noutput_dir: \"/b\"\nlightrag_url: \"http://r\"\n",
    );
    let err = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap_err()
        .to_string();
    // 不校验的话会拖到发请求时报 reqwest 的 "builder error"，看不出是哪个键没配
    assert!(err.contains("mineru_url"), "{err}");
}

#[tokio::test]
async fn 缺_lightrag_url_报错并指出字段名() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(&d, "sync_dir: \"/a\"\noutput_dir: \"/b\"\nmineru_url: \"http://m\"\n");
    let err = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("lightrag_url"), "{err}");
}

#[tokio::test]
async fn 文件不存在_报错要带路径() {
    let mut db = test_db().await;
    let err = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, "/no/such/config.yml")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("/no/such/config.yml"), "报错要带路径: {err}");
}

#[tokio::test]
async fn 语法错误_报错要指出解析失败() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(&d, "sync_dir: [unclosed\n  - a\n");
    let err = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("解析"), "报错要说明是解析失败: {err}");
}

// ---------------------------------------------------------------- run_id

#[tokio::test]
async fn run_id_等于计数器加一_不会覆盖计数器() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = 写配置(&d, "sync_dir: \"/a\"\noutput_dir: \"/b\"\nmineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n");

    let c = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap();
    assert_eq!(c.run_id, 1, "初始计数器是 0");

    // from_file 只读不写：不落库的话每一轮都会拿到同一个 run_id
    assert_eq!(
        rag_sync::database::DBfunc::get_run_counter(&mut db.conn)
            .await
            .unwrap()
            .unwrap()
            .current_run_id,
        0
    );

    // main.rs 的顺序：读到 run_id -> 干活 -> update_run_counter
    rag_sync::database::DBfunc::update_run_counter(&mut db.conn, c.run_id)
        .await
        .unwrap();
    let next = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap();
    assert_eq!(next.run_id, 2, "计数器 +1");
}

#[tokio::test]
async fn 配置里的_run_id_不参与计算() {
    let mut db = test_db().await;
    rag_sync::database::DBfunc::update_run_counter(&mut db.conn, 7)
        .await
        .unwrap();
    let d = tempfile::tempdir().unwrap();
    // 就算配置里写了 run_id: 999，也必须以计数器为准
    let path = 写配置(&d, "sync_dir: \"/a\"\noutput_dir: \"/b\"\nrun_id: 999\nmineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n");
    let c = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap();
    assert_eq!(c.run_id, 8);
}
