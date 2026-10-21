//! Joplin Server 的笔记同步进 LightRAG。
//!
//! 走的是服务端的变更流（delta）：拿到「哪些文件变了」，再按文件名取正文，
//! 正文只在内存里待一下，直接传进知识库 —— 笔记不落盘。
//!
//! 游标存在库里，下一轮接着上轮的游标往下取；游标失效（服务端要求重同步）时
//! 从头再来一遍，靠内容指纹认出「没变的笔记不用重传」。

use crate::database::DBfunc;
use crate::joplin_model::{self, event_type};
use crate::joplin_requests::{JoplinClient, JoplinError, MAX_PAGES};
use crate::lightrag_requests::{ConflictHint, RAGRequest};
use crate::sync_config::SyncConfig;
use crate::{database, http_util, sync_folder};
use std::collections::HashMap;

/// 一轮 Joplin 同步的结果。
#[derive(Debug, Default, Clone, Copy)]
pub struct JoplinSyncSummary {
    /// 传进知识库的笔记数。
    pub uploaded: usize,
    /// 内容没变、跳过的笔记数。
    pub unchanged: usize,
    /// 从知识库里删掉的笔记数。
    pub deleted: usize,
    /// 不是笔记（笔记本、资源、变更记录、历史版本）而跳过的文件数。
    pub skipped: usize,
}

/// 跑一轮 Joplin → LightRAG 同步。config.yml 里没有 joplin 段就直接返回。
pub async fn sync_joplin(
    config: &SyncConfig,
    conn: &mut database::Conn,
) -> Result<JoplinSyncSummary, Box<dyn std::error::Error>> {
    let Some(joplin) = &config.joplin else {
        return Ok(JoplinSyncSummary::default());
    };

    let client = http_util::client()?;
    let jop = JoplinClient::new(&client, &joplin.url, &joplin.email, &joplin.password).await?;

    let saved_cursor = DBfunc::get_joplin_cursor(conn).await?;
    // 从头开始就是全量重放：库里残留的、已经不存在的笔记要一并清掉
    let mut full_replay = saved_cursor.is_none();
    let (events, end_cursor) =
        match collect_events(&jop, saved_cursor.as_deref(), joplin.limit).await {
            Ok(v) => v,
            Err(JoplinError::ResyncRequired) => {
                println!("[Joplin] 游标失效，从头全量同步");
                full_replay = true;
                collect_events(&jop, None, joplin.limit).await?
            }
            Err(e) => return Err(e.into()),
        };

    // 同一个文件的多条事件只留最后一条：最后一条才是它的最新状态
    let mut latest: HashMap<String, joplin_model::JoplinSyncItem> = HashMap::new();
    for event in events {
        latest.insert(event.item_name.clone(), event);
    }
    let mut events: Vec<joplin_model::JoplinSyncItem> = latest.into_values().collect();
    events.sort_by_key(|e| e.updated_time);

    let mut summary = JoplinSyncSummary::default();
    if events.is_empty() {
        save_cursor(conn, end_cursor).await?;
        return Ok(summary);
    }

    let rag = RAGRequest::new(
        &client,
        &config.lightrag_url,
        &config.lightrag_username,
        &config.lightrag_password,
    )
    .await?;

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for event in &events {
        seen.insert(event.item_name.clone());
        match event.r#type {
            event_type::DELETED => {
                if remove_note(&rag, conn, &event.item_name).await? {
                    summary.deleted += 1;
                }
            }
            _ => {
                sync_one_note(&rag, conn, joplin, &jop, event, &mut summary).await?;
            }
        }
    }

    // 全量重放时，库里那些这轮没出现的笔记是「服务端已经没有它了」，一并清掉
    if full_replay {
        cleanup_missing_notes(&rag, conn, &seen).await?;
    }

    save_cursor(conn, end_cursor).await?;
    Ok(summary)
}

/// 拉完整个变更流。返回（事件，末尾的游标）。
/// 游标必须原样传回去：它可能指向一条被压缩掉、从未下发给客户端的事件。
async fn collect_events(
    jop: &JoplinClient,
    cursor: Option<&str>,
    limit: usize,
) -> Result<(Vec<joplin_model::JoplinSyncItem>, Option<String>), JoplinError> {
    let mut events = Vec::new();
    let mut cursor = cursor.map(str::to_string);
    for _ in 0..MAX_PAGES {
        let page = jop.delta(cursor.as_deref(), limit).await?;
        events.extend(page.items);
        let next = page.cursor.clone();
        if !page.has_more {
            return Ok((events, next));
        }
        if next == cursor {
            return Err(JoplinError::Other(
                "服务端一直回 has_more，但游标没有前进".to_string(),
            ));
        }
        cursor = next;
    }
    Err(JoplinError::Other(format!(
        "变更流翻了 {MAX_PAGES} 页仍未结束，疑似服务端一直回 has_more"
    )))
}

/// 记下这一轮的终点游标。下轮从这儿接着取。
async fn save_cursor(
    conn: &mut database::Conn,
    cursor: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(c) = cursor {
        DBfunc::set_joplin_cursor(conn, &c).await?;
    }
    Ok(())
}

/// 同步一篇笔记：取正文、变了就重传。
async fn sync_one_note(
    rag: &RAGRequest<'_>,
    conn: &mut database::Conn,
    joplin: &crate::sync_config::JoplinConfig,
    jop: &JoplinClient,
    event: &joplin_model::JoplinSyncItem,
    summary: &mut JoplinSyncSummary,
) -> Result<(), Box<dyn std::error::Error>> {
    // 资源、变更记录之外的文件名不是 .md，直接跳过，连内容都不用取
    if !event.item_name.ends_with(".md") {
        summary.skipped += 1;
        return Ok(());
    }

    let content = match jop.content(&event.item_name).await {
        Ok(c) => c,
        // 文件已经不在了：当作被删了
        Err(JoplinError::NotFound) => {
            if remove_note(rag, conn, &event.item_name).await? {
                summary.deleted += 1;
            }
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let item = joplin_model::parse_item(&content);
    // 只有笔记进知识库：变更记录、历史版本、笔记本、资源都跳过
    if !item.is_note() {
        summary.skipped += 1;
        return Ok(());
    }
    // 客户端删掉的笔记是软删除：正文还在，但已经不是「用户要的东西」了
    if item.deleted_time() > 0 {
        if remove_note(rag, conn, &event.item_name).await? {
            summary.deleted += 1;
        }
        return Ok(());
    }

    let item_id = item.item_id().unwrap_or(&event.item_id).to_string();
    let body = item.body;
    if body.trim().is_empty() {
        summary.skipped += 1;
        return Ok(());
    }
    let sha256 = sync_folder::sha256_text(&body);
    let upload_name = format!("{}__{}", joplin.alias, event.item_name);
    let existing = DBfunc::get_joplin_note(conn, &event.item_name).await?;

    // 内容与上传名都没变：知识库里已经是这一份，不用重传
    if let Some(note) = &existing
        && note.sha256 == sha256
        && note.upload_name == upload_name
    {
        summary.unchanged += 1;
        return Ok(());
    }

    // 旧文档先让位：同名重传会被 409 拦下
    if let Some(note) = &existing
        && let Some(rag_id) = note.rag_id.as_deref()
        && rag.document_exists(rag_id).await?
    {
        rag.delete_document_when_free(rag_id).await?;
        println!("[Joplin] 已删除旧的知识库文档: {rag_id}");
    }

    let hint = ConflictHint {
        uploaded_sha256: existing.as_ref().map(|n| n.sha256.as_str()),
        rag_id: existing.as_ref().and_then(|n| n.rag_id.as_deref()),
        content_sha256: Some(&sha256),
    };
    // 正文只在内存里，直接进知识库
    let rag_id = rag
        .upload_content_as(&upload_name, body.as_bytes(), Some(&hint))
        .await?;
    DBfunc::upsert_joplin_note(
        conn,
        &item_id,
        &event.item_name,
        &upload_name,
        &sha256,
        Some(&rag_id),
        event.updated_time,
    )
    .await?;
    println!("[Joplin] 已入库: {upload_name}（{item_id}）");
    summary.uploaded += 1;
    Ok(())
}

/// 把一篇笔记从知识库里撤掉，并删掉它的记录。
/// 库里没有这篇笔记时什么都不做，返回 false。
async fn remove_note(
    rag: &RAGRequest<'_>,
    conn: &mut database::Conn,
    item_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let Some(note) = DBfunc::get_joplin_note(conn, item_name).await? else {
        return Ok(false);
    };
    if let Some(rag_id) = note.rag_id.as_deref()
        && rag.document_exists(rag_id).await?
    {
        rag.delete_document_when_free(rag_id).await?;
        println!("[Joplin] 已删除知识库文档: {rag_id}");
    }
    DBfunc::delete_joplin_note(conn, item_name).await?;
    Ok(true)
}

/// 全量重放后清掉「这轮没出现」的笔记：服务端已经没有它们了。
async fn cleanup_missing_notes(
    rag: &RAGRequest<'_>,
    conn: &mut database::Conn,
    seen: &std::collections::HashSet<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let notes = DBfunc::list_joplin_notes(conn).await?;
    for note in notes {
        if seen.contains(&note.item_name) {
            continue;
        }
        if let Some(rag_id) = note.rag_id.as_deref()
            && rag.document_exists(rag_id).await?
        {
            rag.delete_document_when_free(rag_id).await?;
            println!("[Joplin] 已删除知识库文档: {rag_id}");
        }
        DBfunc::delete_joplin_note(conn, &note.item_name).await?;
    }
    Ok(())
}
