use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/*
'{"email":"cmach_socket@outlook.com","password":"<密码>"}'
*/
#[derive(Serialize)]
pub struct JoplinLoginRequest {
    pub email: String,
    pub password: String,
}
/*```json
{"id":"BAeoG5RnljxeWuqDJMqd7D","user_id":"8iI9YHJnorbl2qI39ylj08"}
``` */

#[derive(Deserialize)]
pub struct JoplinLoginResponse {
    pub id: String,
    pub user_id: String,
}

/*
```json
{
  "items": [
    {
      "id": "5cP8NxGL761cQxFq9XR8lj",
      "item_id": "JoSSy9ABC79teutozvOktW",
      "item_name": "132e455e2bf54a0aa5052039243ee21f.md",
      "previous_share_id": "",
      "type": 1,
      "updated_time": 1792543798451,
      "jop_updated_time": 1706701910785
    }
  ],
  "cursor": "DZI6Z8mKClGkXza32tdlj2",
  "has_more": true
}
```
*/

#[derive(Deserialize, Debug, Clone)]
pub struct JoplinSyncItem {
    pub id: String,
    pub item_id: String,
    pub item_name: String,
    #[serde(default)]
    pub previous_share_id: String,
    pub r#type: u8,
    pub updated_time: i64,
    #[serde(default)]
    pub jop_updated_time: Option<i64>,
}

#[derive(Deserialize)]
pub struct JoplinSyncResponse {
    pub items: Vec<JoplinSyncItem>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// delta 事件的类型：新建 / 更新 / 删除。
pub mod event_type {
    pub const CREATED: u8 = 1;
    pub const UPDATED: u8 = 2;
    pub const DELETED: u8 = 3;
}

/// 服务端的错误体：`{"error": "...", "code": "resyncRequired"}`。
#[derive(Deserialize, Debug)]
pub struct JoplinErrorResponse {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub code: Option<String>,
}

/// item 的类型（`type_`）：只有笔记要进知识库。
pub mod item_type {
    /// 笔记
    pub const NOTE: i64 = 1;
    /// 笔记本
    pub const NOTEBOOK: i64 = 2;
    /// 资源
    pub const RESOURCE: i64 = 4;
    /// 标签
    pub const TAG: i64 = 5;
    /// 笔记的历史版本
    pub const REVISION: i64 = 6;
    /// 变更记录
    pub const CHANGE: i64 = 13;
}

/// 切开后的 item：正文 + 末尾的元信息块。
#[derive(Debug, Clone)]
pub struct JoplinItem {
    /// 正文（元信息块之前的部分）。
    pub body: String,
    /// 元信息块，键是元信息的键（`id` / `parent_id` / `type_` / `deleted_time` …）。
    pub metadata: HashMap<String, String>,
}

impl JoplinItem {
    /// item 的 id，即文件名里的那段十六进制。
    pub fn item_id(&self) -> Option<&str> {
        self.metadata.get("id").map(String::as_str)
    }

    /// 挂在哪个笔记本上。
    pub fn parent_id(&self) -> Option<&str> {
        self.metadata.get("parent_id").map(String::as_str)
    }

    /// item 的类型（`type_`）。
    pub fn item_type(&self) -> Option<i64> {
        self.metadata
            .get("type_")
            .and_then(|v| v.trim().parse::<i64>().ok())
    }

    /// 软删除的时间戳。非 0 表示这条笔记已被客户端删掉（还没到彻底清理的那天）。
    pub fn deleted_time(&self) -> i64 {
        self.metadata
            .get("deleted_time")
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(0)
    }

    /// 是不是一条笔记（只有笔记进知识库）。
    pub fn is_note(&self) -> bool {
        self.item_type() == Some(item_type::NOTE)
    }

    /// 标题：正文第一行。笔记的标题没有单独的元信息字段。
    pub fn title(&self) -> &str {
        self.body.lines().next().unwrap_or("").trim()
    }
}

/// 一行是不是 `key: value` 形态（元信息块的行都是这个形态）。
fn is_key_value(line: &str) -> bool {
    match line.split_once(':') {
        Some((key, _)) => {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// 取 `key: value` 里的 key。
fn key_of(line: &str) -> &str {
    line.split_once(':').map(|(k, _)| k).unwrap_or("")
}

/// 取 `key: value` 里的 value。
fn value_of(line: &str) -> String {
    line.split_once(':')
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default()
}

/// 切开「正文 + 元信息块」。
///
/// 末尾是一段连续的 `key: value` 行；这段里必须同时有 `id`、`parent_id`、`type_`
/// 三个键，否则整份文件都当正文 —— 正文里出现 `备注: xxx` 这种行时，
/// 只看「结尾是 key: value」会把正文尾巴切掉。
///
/// 块的起点取这段里最后一个键恰为 `id` 的行：正文里若有一行以 `id: ` 开头，
/// 它在块的上面，不该被当成元信息。
pub fn parse_item(content: &str) -> JoplinItem {
    // 末尾的空行不是元信息的一行，去掉再找块的边界
    let trimmed = content.trim_end();
    let lines: Vec<&str> = trimmed.split('\n').collect();
    let whole = || JoplinItem {
        body: trimmed.to_string(),
        metadata: HashMap::new(),
    };

    // 从末尾往前找连续的 `key: value` 行
    let mut run_start = lines.len();
    while run_start > 0 && is_key_value(lines[run_start - 1]) {
        run_start -= 1;
    }
    if run_start == lines.len() {
        return whole();
    }

    // 块的起点：这段里最后一个键为 `id` 的行
    let Some(block_start) = (run_start..lines.len())
        .rev()
        .find(|&i| key_of(lines[i]) == "id")
    else {
        return whole();
    };

    let metadata: HashMap<String, String> = lines[block_start..]
        .iter()
        .filter(|line| is_key_value(line))
        .map(|line| (key_of(line).to_string(), value_of(line)))
        .collect();

    // 三个键缺一不可，缺了就当没有元信息块
    if !metadata.contains_key("parent_id") || !metadata.contains_key("type_") {
        return whole();
    }

    let body = lines[..block_start].join("\n").trim_end().to_string();
    JoplinItem { body, metadata }
}
