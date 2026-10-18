#![allow(non_snake_case)]
//! DBfunc 的行为测试。跑的是真 SQLite + 真 migration。
//!
//! 这里固定的是「改了 DB 层会破坏谁」的契约：同步流程靠 sha256 判断文件是否变化，
//! 靠 run_id 判断文件是否还在本轮出现过。这两个字段写错方向，症状是
//! 「用户文档被删」而不是报错，所以每条都单独钉死。

mod common;

use common::test_db;
use rag_sync::database::DBfunc;
use rag_sync::db_model::File;

async fn 一个文件(conn: &mut rag_sync::database::Conn) -> File {
    DBfunc::insert_file(conn, "/sync/a.pdf", "sha-a", 1, Some("doc-1"))
        .await
        .unwrap();
    DBfunc::get_file(conn, "/sync/a.pdf").await.unwrap().unwrap()
}

// ---------------------------------------------------------------- 增删查

#[tokio::test]
async fn insert_后可原样读回() {
    let mut db = test_db().await;
    let file = 一个文件(&mut db.conn).await;
    assert_eq!(file.path, "/sync/a.pdf");
    assert_eq!(file.sha256, "sha-a");
    assert_eq!(file.rag_id.as_deref(), Some("doc-1"));
}

#[tokio::test]
async fn file_exists_对不存在的路径为_false() {
    let mut db = test_db().await;
    assert!(!DBfunc::file_exists(&mut db.conn, "/sync/a.pdf").await.unwrap());
    一个文件(&mut db.conn).await;
    assert!(DBfunc::file_exists(&mut db.conn, "/sync/a.pdf").await.unwrap());
}

#[tokio::test]
async fn get_不存在的行返回_None_而不是报错() {
    let mut db = test_db().await;
    assert_eq!(DBfunc::get_sha256(&mut db.conn, "/nope").await.unwrap(), None);
    assert_eq!(DBfunc::get_file_run_id(&mut db.conn, "/nope").await.unwrap(), None);
    assert!(DBfunc::get_file(&mut db.conn, "/nope").await.unwrap().is_none());
    assert!(!DBfunc::file_exists(&mut db.conn, "/nope").await.unwrap());
}

// ---------------------------------------------------------------- 更新

#[tokio::test]
async fn 更新_sha256_只动目标行() {
    let mut db = test_db().await;
    db.insert_file_row(2, "/sync/b.pdf", "sha-b", 7, None).await;
    一个文件(&mut db.conn).await;

    DBfunc::update_sha256(&mut db.conn, "/sync/a.pdf", "sha-a2")
        .await
        .unwrap();

    assert_eq!(
        DBfunc::get_sha256(&mut db.conn, "/sync/a.pdf").await.unwrap(),
        Some("sha-a2".to_string())
    );
    // 兄弟行不能被带偏
    assert_eq!(
        DBfunc::get_sha256(&mut db.conn, "/sync/b.pdf").await.unwrap(),
        Some("sha-b".to_string())
    );
}

#[tokio::test]
async fn 更新_rag_id_支持写回_None() {
    let mut db = test_db().await;
    一个文件(&mut db.conn).await;

    DBfunc::update_rag_id(&mut db.conn, "/sync/a.pdf", None)
        .await
        .unwrap();
    let f = DBfunc::get_file(&mut db.conn, "/sync/a.pdf").await.unwrap().unwrap();
    assert_eq!(f.rag_id, None);

    DBfunc::update_rag_id(&mut db.conn, "/sync/a.pdf", Some("doc-2"))
        .await
        .unwrap();
    let f = DBfunc::get_file(&mut db.conn, "/sync/a.pdf").await.unwrap().unwrap();
    assert_eq!(f.rag_id.as_deref(), Some("doc-2"));
}

#[tokio::test]
async fn 更新_run_id_只动目标行() {
    let mut db = test_db().await;
    db.insert_file_row(2, "/sync/b.pdf", "sha-b", 7, None).await;
    一个文件(&mut db.conn).await;

    DBfunc::update_file_run_id(&mut db.conn, "/sync/a.pdf", 99)
        .await
        .unwrap();

    assert_eq!(
        DBfunc::get_file_run_id(&mut db.conn, "/sync/a.pdf").await.unwrap(),
        Some(99)
    );
    assert_eq!(
        DBfunc::get_file_run_id(&mut db.conn, "/sync/b.pdf").await.unwrap(),
        Some(7)
    );
}

// ---------------------------------------------------------------- run_counter

#[tokio::test]
async fn migration_已插入_单行计数器且为_0() {
    let mut db = test_db().await;
    let c = DBfunc::get_run_counter(&mut db.conn).await.unwrap();
    assert_eq!(c.map(|c| c.current_run_id), Some(0));
}

#[tokio::test]
async fn 计数器可更新且只有一个生效行() {
    let mut db = test_db().await;
    DBfunc::update_run_counter(&mut db.conn, 42).await.unwrap();
    let c = DBfunc::get_run_counter(&mut db.conn).await.unwrap().unwrap();
    assert_eq!(c.current_run_id, 42);
    assert_eq!(c.id, 1, "表结构限定 id 恒为 1");
}

// ---------------------------------------------------------------- run_id 范围查询

#[tokio::test]
async fn 查旧文件_严格小于_并按路径排序稳定() {
    let mut db = test_db().await;
    db.insert_file_row(1, "/sync/old1.pdf", "s1", 3, None).await;
    db.insert_file_row(2, "/sync/cur.pdf", "s2", 5, None).await;
    db.insert_file_row(3, "/sync/old2.pdf", "s3", 4, None).await;
    // 同 run_id 的边界：5 不该被算成旧
    db.insert_file_row(4, "/sync/cur2.pdf", "s4", 5, None).await;

    let old = DBfunc::found_file_less_than_run_id(&mut db.conn, 5)
        .await
        .unwrap();
    let paths: Vec<&str> = old.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["/sync/old1.pdf", "/sync/old2.pdf"]);
}

#[tokio::test]
async fn 查旧文件_返回完整行_含_rag_id_供删除用() {
    let mut db = test_db().await;
    db.insert_file_row(1, "/sync/old.pdf", "s1", 1, Some("doc-9"))
        .await;
    let old = DBfunc::found_file_less_than_run_id(&mut db.conn, 2)
        .await
        .unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].rag_id.as_deref(), Some("doc-9"));
    assert_eq!(old[0].sha256, "s1");
}

#[tokio::test]
async fn 查旧文件_没有旧文件时返回空vec_而不是报错() {
    let mut db = test_db().await;
    // 用 fixture 而不是 insert_file：后者不写 run_id，测不出查询语义
    db.insert_file_row(1, "/sync/cur.pdf", "s1", 1, None).await;
    let old = DBfunc::found_file_less_than_run_id(&mut db.conn, 1)
        .await
        .unwrap();
    assert!(old.is_empty());
}

#[tokio::test]
async fn 查旧文件_本轮见过的文件不会被判为旧() {
    let mut db = test_db().await;
    db.insert_file_row(1, "/sync/a.pdf", "s1", 4, None).await;
    // 本轮 run_id == 4，严格小于的查询必须放过它
    assert!(DBfunc::found_file_less_than_run_id(&mut db.conn, 4)
        .await
        .unwrap()
        .is_empty());
    // 但下一轮（run_id 5）它就成了旧文件
    assert_eq!(
        DBfunc::found_file_less_than_run_id(&mut db.conn, 5)
            .await
            .unwrap()
            .len(),
        1
    );
}
