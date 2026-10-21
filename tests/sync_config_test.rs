//! 配置加载与 run_id 的推导。run_id 既决定「哪些文件算这轮新增」，
//! 又决定 `delete_old_files` 会删谁，推错一位就是误删。

mod common;

use common::test_db;

/// 一份最小可用配置：`head` 之后接上就行。
const MINIMAL_TAIL: &str =
    "output_dir: \"/data/out\"\nmineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n";

fn folders_yaml(entries: &[(&str, &str)]) -> String {
    let mut s = String::from("sync_folders:\n");
    for (name, path) in entries {
        s.push_str(&format!("  - name: \"{name}\"\n    path: \"{path}\"\n"));
    }
    s
}

fn write_config(dir: &tempfile::TempDir, body: &str) -> String {
    let p = dir.path().join("config.yml");
    std::fs::write(&p, body).unwrap();
    p.to_string_lossy().into_owned()
}

async fn load(body: &str) -> Result<rag_sync::sync_config::SyncConfig, String> {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = write_config(&d, body);
    rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn loads_all_fields() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let path = write_config(
        &d,
        "sync_folders:\n  - name: \"work\"\n    path: \"/data/sync\"\n\
         output_dir: \"/data/out\"\nmineru_url: \"http://m:9777\"\n\
         lightrag_url: \"http://r:9621\"\nlightrag_username: \"u\"\nlightrag_password: \"p\"\n",
    );
    let c = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap();
    assert_eq!(c.sync_folders.len(), 1);
    assert_eq!(c.sync_folders[0].name, "work");
    assert_eq!(c.sync_folders[0].path, "/data/sync");
    assert_eq!(c.output_dir, "/data/out");
    assert_eq!(c.mineru_url, "http://m:9777");
    assert_eq!(c.lightrag_url, "http://r:9621");
    assert_eq!(c.lightrag_username, "u");
    assert_eq!(c.lightrag_password, "p");
}

/// 多文件夹是这个功能的主场景：别名按配置顺序原样保留，产物目录与上传名都靠它。
#[tokio::test]
async fn loads_every_folder_with_its_alias() {
    let c = load(
        &(folders_yaml(&[("work", "/data/work"), ("personal", "/data/personal")]) + MINIMAL_TAIL),
    )
    .await
    .unwrap();
    let got: Vec<(&str, &str)> = c
        .sync_folders
        .iter()
        .map(|f| (f.name.as_str(), f.path.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![("work", "/data/work"), ("personal", "/data/personal")]
    );
    assert_eq!(c.alias_list(), "work / personal");
}

#[tokio::test]
async fn missing_keys_use_defaults() {
    let c = load(
        &(folders_yaml(&[("work", "/data/sync")])
            + "output_dir: \"/data/out\"\nmineru_url: \"http://m:9777\"\nlightrag_url: \"http://r:9621\"\n"))
            .await
            .unwrap();
    assert_eq!(c.lightrag_username, "");
    assert_eq!(c.lightrag_password, "");
    // run_id 被计数器 +1 无条件覆盖，serde 上的 `default = "zero"` 实际不可达
    assert_eq!(c.run_id, 1);
    // 并发默认 1：老配置里没有这两个键，行为不能变
    assert_eq!(c.mineru_concurrency, 1);
    assert_eq!(c.lightrag_concurrency, 1);
}

/// 并发数从配置里读：调了没反应等于没这个配置。
#[tokio::test]
async fn concurrency_is_configurable() {
    let c = load(
        &(folders_yaml(&[("work", "/data/sync")])
            + "output_dir: \"/data/out\"\nmineru_url: \"http://m:9777\"\nlightrag_url: \"http://r:9621\"\n\
              mineru_concurrency: 2\nlightrag_concurrency: 4\n")
    )
    .await
    .unwrap();
    assert_eq!(c.mineru_concurrency, 2);
    assert_eq!(c.lightrag_concurrency, 4);
}

#[tokio::test]
async fn missing_sync_folders_names_the_field() {
    let err = load(MINIMAL_TAIL).await.unwrap_err();
    assert!(err.contains("sync_folders"), "报错要说清缺哪个字段: {err}");
}

/// 旧的单目录写法 `sync_dir` 不再支持，升级的人第一眼就要看到怎么改。
#[tokio::test]
async fn legacy_sync_dir_is_rejected_with_migration_hint() {
    let err = load(&format!("sync_dir: \"/data/sync\"\n{MINIMAL_TAIL}"))
        .await
        .unwrap_err();
    assert!(err.contains("sync_folders"), "{err}");
    assert!(err.contains("sync_dir"), "要说清旧写法为什么不行了: {err}");
    assert!(err.contains("name"), "要给出新写法的样子: {err}");
}

#[tokio::test]
async fn empty_sync_folders_list_is_missing() {
    let err = load(&format!("sync_folders: []\n{MINIMAL_TAIL}"))
        .await
        .unwrap_err();
    assert!(err.contains("sync_folders"), "空列表应被当成缺失: {err}");
}

#[tokio::test]
async fn missing_output_dir_names_the_field() {
    let err = load(
        &(folders_yaml(&[("work", "/data/sync")])
            + "mineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n"),
    )
    .await
    .unwrap_err();
    assert!(err.contains("output_dir"), "报错要说清缺哪个字段: {err}");
}

#[tokio::test]
async fn folder_without_name_is_rejected() {
    let err = load(&format!(
        "sync_folders:\n  - path: \"/data/sync\"\n{MINIMAL_TAIL}"
    ))
    .await
    .unwrap_err();
    assert!(err.contains("name"), "缺别名要说清是哪个字段: {err}");
}

#[tokio::test]
async fn folder_without_path_is_rejected() {
    let err = load(&format!(
        "sync_folders:\n  - name: \"work\"\n{MINIMAL_TAIL}"
    ))
    .await
    .unwrap_err();
    assert!(err.contains("path"), "缺目录要说清是哪个字段: {err}");
}

#[tokio::test]
async fn empty_path_counts_as_missing() {
    let err = load(&format!(
        "sync_folders:\n  - name: \"work\"\n    path: \"\"\n{MINIMAL_TAIL}"
    ))
    .await
    .unwrap_err();
    assert!(err.contains("path"), "空串应被当成缺失: {err}");
}

/// 别名会变成子目录名与上传名前缀，含分隔符就会把产物写到 output_dir 之外。
#[tokio::test]
async fn alias_with_path_separator_is_rejected() {
    for body in [
        // 双引号标量里 `\` 是转义符，反斜杠那一支改用单引号写
        format!("sync_folders:\n  - name: \"a/b\"\n    path: \"/data/sync\"\n{MINIMAL_TAIL}"),
        format!("sync_folders:\n  - name: 'a\\b'\n    path: \"/data/sync\"\n{MINIMAL_TAIL}"),
    ] {
        let err = load(&body).await.unwrap_err();
        assert!(err.contains("路径分隔符"), "不该被放行: {err}");
    }
}

/// `__` 是上传名的分隔符：别名 `a__b` + `c.pdf` 会和别名 `a` + `b__c.pdf` 撞成同一个键。
#[tokio::test]
async fn alias_with_double_underscore_is_rejected() {
    let err = load(&(folders_yaml(&[("a__b", "/data/sync")]) + MINIMAL_TAIL))
        .await
        .unwrap_err();
    assert!(err.contains("__"), "{err}");
}

#[tokio::test]
async fn alias_with_whitespace_is_rejected() {
    let err = load(&(folders_yaml(&[("my work", "/data/sync")]) + MINIMAL_TAIL))
        .await
        .unwrap_err();
    assert!(err.contains("空白"), "{err}");
}

#[tokio::test]
async fn dot_aliases_are_rejected() {
    for alias in [".", ".."] {
        let err = load(&(folders_yaml(&[(alias, "/data/sync")]) + MINIMAL_TAIL))
            .await
            .unwrap_err();
        assert!(err.contains("别名"), "{alias:?} 不该被放行: {err}");
    }
}

#[tokio::test]
async fn duplicate_alias_is_rejected() {
    let err = load(&(folders_yaml(&[("work", "/data/a"), ("work", "/data/b")]) + MINIMAL_TAIL))
        .await
        .unwrap_err();
    assert!(
        err.contains("重复"),
        "撞名会让产物目录与上传名都不唯一: {err}"
    );
}

/// 同一目录配两个别名：同一个文件会被扫两次，两份产物互相覆盖、上传时 409 互删。
#[tokio::test]
async fn same_path_twice_is_rejected() {
    let err =
        load(&(folders_yaml(&[("work", "/data/sync"), ("alias2", "/data/sync")]) + MINIMAL_TAIL))
            .await
            .unwrap_err();
    assert!(err.contains("同一个目录"), "{err}");
}

#[tokio::test]
async fn overlapping_paths_are_rejected() {
    let err = load(&(folders_yaml(&[("all", "/data"), ("work", "/data/work")]) + MINIMAL_TAIL))
        .await
        .unwrap_err();
    assert!(err.contains("重叠"), "{err}");
}

/// 尾部斜杠不是新的目录：不去掉的话 `starts_with` 判不出来，重叠就漏了。
#[tokio::test]
async fn overlapping_paths_with_trailing_slash_are_rejected() {
    let err = load(&(folders_yaml(&[("all", "/data/"), ("work", "/data/work")]) + MINIMAL_TAIL))
        .await
        .unwrap_err();
    assert!(err.contains("重叠"), "{err}");
}

/// 字符串前缀不是目录包含关系：`/data/a` 与 `/data/ab` 互不重叠。
#[tokio::test]
async fn string_prefix_is_not_overlap() {
    let c = load(&(folders_yaml(&[("a", "/data/a"), ("ab", "/data/ab")]) + MINIMAL_TAIL))
        .await
        .unwrap();
    assert_eq!(c.sync_folders.len(), 2);
}

#[tokio::test]
async fn missing_mineru_url_names_the_field() {
    let err = load(
        &(folders_yaml(&[("work", "/a")]) + "output_dir: \"/b\"\nlightrag_url: \"http://r\"\n"),
    )
    .await
    .unwrap_err();
    // 不校验的话会拖到发请求时报 reqwest 的 "builder error"，看不出是哪个键没配
    assert!(err.contains("mineru_url"), "{err}");
}

#[tokio::test]
async fn missing_lightrag_url_names_the_field() {
    let err =
        load(&(folders_yaml(&[("work", "/a")]) + "output_dir: \"/b\"\nmineru_url: \"http://m\"\n"))
            .await
            .unwrap_err();
    assert!(err.contains("lightrag_url"), "{err}");
}

#[tokio::test]
async fn missing_config_file_reports_path() {
    let mut db = test_db().await;
    let err = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, "/no/such/config.yml")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("/no/such/config.yml"), "报错要带路径: {err}");
}

#[tokio::test]
async fn syntax_error_reports_parse_failure() {
    let err = load("sync_folders: [unclosed\n  - a\n").await.unwrap_err();
    assert!(err.contains("解析"), "报错要说明是解析失败: {err}");
}

// 归属

/// 清理时只有一条 DB 记录（源绝对路径），要靠它反推别名子目录。
/// 直接构造 config：重叠目录在加载时就会被拒，这里验的是归属规则本身。
#[test]
fn folder_of_picks_the_most_specific_root() {
    let c = rag_sync::sync_config::SyncConfig {
        sync_folders: vec![
            rag_sync::sync_config::SyncFolder {
                name: "work".into(),
                path: "/data/work".into(),
            },
            rag_sync::sync_config::SyncFolder {
                name: "work_deep".into(),
                path: "/data/work/deep".into(),
            },
        ],
        output_dir: "/out".into(),
        mineru_url: "http://m".into(),
        run_id: 1,
        lightrag_password: "p".into(),
        lightrag_url: "http://r".into(),
        lightrag_username: "u".into(),
        joplin: None,
        mineru_concurrency: 1,
        lightrag_concurrency: 1,
    };

    assert_eq!(
        c.folder_of(std::path::Path::new("/data/work/a.pdf"))
            .map(|f| f.name.as_str()),
        Some("work")
    );
    assert_eq!(
        c.folder_of(std::path::Path::new("/data/work/deep/a.pdf"))
            .map(|f| f.name.as_str()),
        Some("work_deep"),
        "更深的目录要赢，否则同一个文件会算出两个产物路径"
    );
    // 目录外不该被认领，否则清理时会算出错的产物路径
    assert!(
        c.folder_of(std::path::Path::new("/data/other/a.pdf"))
            .is_none()
    );
    // 字符串前缀不算包含
    assert!(
        c.folder_of(std::path::Path::new("/data/working/a.pdf"))
            .is_none()
    );
}

// run_id

#[tokio::test]
async fn run_id_is_counter_plus_one() {
    let mut db = test_db().await;
    let d = tempfile::tempdir().unwrap();
    let body = folders_yaml(&[("work", "/a")]) + MINIMAL_TAIL;
    let path = write_config(&d, &body);

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
async fn configured_run_id_is_ignored() {
    let mut db = test_db().await;
    rag_sync::database::DBfunc::update_run_counter(&mut db.conn, 7)
        .await
        .unwrap();
    let d = tempfile::tempdir().unwrap();
    // 就算配置里写了 run_id: 999，也必须以计数器为准
    let body = folders_yaml(&[("work", "/a")])
        + "output_dir: \"/b\"\nrun_id: 999\nmineru_url: \"http://m\"\nlightrag_url: \"http://r\"\n";
    let path = write_config(&d, &body);
    let c = rag_sync::sync_config::SyncConfig::from_file(&mut db.conn, &path)
        .await
        .unwrap();
    assert_eq!(c.run_id, 8);
}

// Joplin 段

/// joplin 段是可选的：没有它就只做目录同步。
#[tokio::test]
async fn joplin_section_is_optional() {
    let c = load(&(folders_yaml(&[("work", "/a")]) + MINIMAL_TAIL))
        .await
        .unwrap();
    assert!(c.joplin.is_none(), "没配 joplin 段就不该有 Joplin 配置");
}

#[tokio::test]
async fn joplin_section_is_loaded_with_defaults() {
    let body = folders_yaml(&[("work", "/a")])
        + MINIMAL_TAIL
        + "joplin:\n  url: \"https://joplin.example.com\"\n  email: \"a@b.c\"\n  password: \"p\"\n";
    let c = load(&body).await.unwrap();
    let j = c.joplin.expect("配了 joplin 段就该有配置");
    assert_eq!(j.url, "https://joplin.example.com");
    assert_eq!(j.email, "a@b.c");
    assert_eq!(j.password, "p");
    assert_eq!(j.alias, "joplin", "别名有默认值");
    assert_eq!(j.limit, 100, "每页条数有默认值");
}

/// 地址末尾的斜杠会和接口路径拼成 `//api/...`。
#[tokio::test]
async fn joplin_url_trailing_slash_is_trimmed() {
    let body = folders_yaml(&[("work", "/a")])
        + MINIMAL_TAIL
        + "joplin:\n  url: \"https://joplin.example.com/\"\n  email: \"a@b.c\"\n  password: \"p\"\n";
    let c = load(&body).await.unwrap();
    assert_eq!(c.joplin.unwrap().url, "https://joplin.example.com");
}

#[tokio::test]
async fn joplin_missing_field_names_it() {
    for (缺, 段) in [
        ("url", "joplin:\n  email: \"a@b.c\"\n  password: \"p\"\n"),
        ("email", "joplin:\n  url: \"http://j\"\n  password: \"p\"\n"),
        (
            "password",
            "joplin:\n  url: \"http://j\"\n  email: \"a@b.c\"\n",
        ),
    ] {
        let body = folders_yaml(&[("work", "/a")]) + MINIMAL_TAIL + 段;
        let err = load(&body).await.unwrap_err();
        assert!(err.contains(缺), "报错要说清缺哪个字段: {err}");
    }
}

/// 别名会变成上传文件名的前缀，非法字符要在这里拦下。
#[tokio::test]
async fn joplin_alias_with_double_underscore_is_rejected() {
    let body = folders_yaml(&[("work", "/a")])
        + MINIMAL_TAIL
        + "joplin:\n  url: \"http://j\"\n  email: \"a@b.c\"\n  password: \"p\"\n  alias: \"a__b\"\n";
    let err = load(&body).await.unwrap_err();
    assert!(err.contains("__"), "{err}");
}
