//! Joplin item 的解析：把「正文 + 末尾的元信息块」切开。
//!
//! 切错了，传进知识库的就是错的，所以边界情况都单独钉住。

use rag_sync::joplin_model::{item_type, parse_item};

/// 一条笔记的原文：正文 + 元信息块。
const ITEM: &str = "优化建图方法\n\n目前只会线段树优化建图\n\n\
    id: 98f2e8b73e3043f1bced03d6ff497429\n\
    parent_id: 97851e06ae0c4b7096a1f4a6228f2168\n\
    created_time: 2024-01-31T00:00:00.000Z\n\
    updated_time: 2024-02-01T00:00:00.000Z\n\
    encryption_applied: 0\n\
    type_: 1";

#[test]
fn body_and_metadata_are_split() {
    let item = parse_item(ITEM);
    assert_eq!(item.body, "优化建图方法\n\n目前只会线段树优化建图");
    assert_eq!(item.item_id(), Some("98f2e8b73e3043f1bced03d6ff497429"));
    assert_eq!(item.parent_id(), Some("97851e06ae0c4b7096a1f4a6228f2168"));
    assert_eq!(item.item_type(), Some(item_type::NOTE));
    assert!(item.is_note());
    assert_eq!(item.title(), "优化建图方法");
}

/// 末尾的空行不是元信息的一行，不能因此认不出块。
#[test]
fn trailing_newline_does_not_break_the_split() {
    let item = parse_item(&format!("{ITEM}\n"));
    assert_eq!(item.body, "优化建图方法\n\n目前只会线段树优化建图");
    assert!(item.is_note());
}

/// 正文里也有 `key: value` 的行，不能被当成元信息切走。
#[test]
fn key_value_line_in_body_is_kept() {
    let content = "标题\n\nid: 这是正文里的行\n注意: 别切我\n\nid: real\nparent_id: p\ntype_: 1";
    let item = parse_item(content);
    assert_eq!(item.body, "标题\n\nid: 这是正文里的行\n注意: 别切我");
    assert_eq!(item.item_id(), Some("real"));
}

/// 没有元信息块时整份都是正文。
#[test]
fn content_without_metadata_block_is_all_body() {
    let content = "只有正文\n\n备注: xxx";
    let item = parse_item(content);
    assert_eq!(item.body, content);
    assert!(item.metadata.is_empty());
    assert!(!item.is_note());
}

/// 块里少了 `id` / `parent_id` / `type_` 三个键之一，就不算元信息块。
#[test]
fn incomplete_metadata_block_is_not_metadata() {
    let content = "正文\n\nid: x\ntype_: 1";
    let item = parse_item(content);
    assert_eq!(item.body, content);
    assert!(!item.is_note());
}

/// 变更记录（type_ 13）与历史版本（type_ 6）不是笔记。
#[test]
fn change_records_are_not_notes() {
    let content = "id: e95e148230c4408fa309b22f3bd6a9fb\nparent_id: \nitem_type: 1\n\
                  title_diff: \"[]\"\nbody_diff: \"[]\"\ntype_: 13";
    let item = parse_item(content);
    assert_eq!(item.item_type(), Some(item_type::CHANGE));
    assert!(!item.is_note());
    assert!(item.body.is_empty(), "变更记录没有正文");
}

/// 客户端删掉的笔记是软删除：deleted_time 非 0。
#[test]
fn deleted_time_is_read_from_metadata() {
    let content = format!("{ITEM}\ndeleted_time: 1792551787908");
    let item = parse_item(&content);
    assert_eq!(item.deleted_time(), 1_792_551_787_908);
    assert_eq!(parse_item(ITEM).deleted_time(), 0);
}
