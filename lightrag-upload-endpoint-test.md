# LightRAG `/documents/upload` 接口测试报告

测试时间：2026-10-17（2026-10-18 补充认证与删除章节）
目标服务：`http://192.168.2.7:9621`（LightRAG Server，`core_version 1.5.7`，`api_version 0344`，WebUI 标题 "cmach_socket Graph KB"）
被测端点：`POST /documents/upload` — *Upload To Input Dir*

本文中所有请求均基于实测；标注「未验证」的部分说明见文末。

## 1. 接口契约

以下来自服务端 `/openapi.json`：

| 项 | 值 |
| --- | --- |
| 方法与路径 | `POST /documents/upload` |
| 请求体 | `multipart/form-data`，必填字段 `file` |
| 文件类型判定 | **仅按扩展名**，不读 part 的 `Content-Type` |
| 认证 | `Authorization: Bearer <token>`（`OAuth2PasswordBearer`，tokenUrl 为 `/login`） |
| 成功响应 | `200` + `InsertResponse`：`{status, message, track_id}`，`status` 枚举 `success / partial_success / failure` |
| 其他响应 | `422` 参数校验失败、`400` 不支持的文件类型、`401` 未认证、`409` 同名冲突或独占写状态、`413` 超限 |
| 大小限制 | `MAX_UPLOAD_SIZE`，默认 100 MB，超限返回 `413` |

处理是**异步**的：上传只负责把文件写进 input 目录并立刻返回 `track_id`，真正的解析与图谱抽取由后台流水线完成，需要轮询 `GET /documents/track_status/{track_id}` 拿最终结果。

## 2. 认证

### 2.1 请求

`POST /login`，body 是 **`application/x-www-form-urlencoded`**，不是 JSON —— 就是一行纯文本 `键=值&键=值`，没有花括号没有引号。

```bash
curl -X POST http://192.168.2.7:9621/login \
  -H 'Content-Type: application/x-www-form-urlencoded' \
  -d 'grant_type=password&username=<user>&password=<pwd>'
```

| 参数 | 必填 | 说明 |
| --- | --- | --- |
| `grant_type` | 是 | 固定 `password` |
| `username` | 是 | |
| `password` | 是 | |

响应 `200`：

```json
{
  "access_token": "<JWT, 长度 199>",
  "token_type": "bearer",
  "auth_configured": true,
  "auth_mode": "enabled",
  "core_version": "1.5.7",
  "api_version": "0344"
}
```

注意：字段名是 **`access_token`** 而不是 `token`；`expires_in` 字段不存在，看不出有过期时间。

### 2.2 状态确认

`GET /auth-status`（免鉴权）：

```json
{
  "auth_configured": true,
  "auth_mode": "enabled",
  "core_version": "1.5.7",
  "api_version": "0344",
  "webui_title": "cmach_socket Graph KB",
  "webui_description": "Simple and Fast Graph Based RAG System",
  "ai_content_notice_enabled": false
}
```

未带 token 访问受保护端点：

```
GET /documents/supported_file_types   → HTTP 401
{"detail":"No credentials provided. Please login."}
```

### 2.3 凭据

`config.yml.example` 里写的 `admin/admin` 是占位值，实测返回：

```
HTTP 401
{"detail":"Incorrect credentials"}
```

## 3. 正常上传测试

### 3.1 完整请求

`-F` 即 `--form`，发 `multipart/form-data`。`file=@路径` 中 `@` 表示"读这个文件的内容上传"，否则上传的是字面量。

```bash
curl -X POST http://192.168.2.7:9621/documents/upload \
  -H "Authorization: Bearer $TOKEN" \
  -F "file=@/home/cmach_socket/projects/rag-sync/docs/configuration.md"
```

等价的 HTTP 报文：

```http
POST /documents/upload HTTP/1.1
Host: 192.168.2.7:9621
Authorization: Bearer <token>
Content-Type: multipart/form-data; boundary=----WebKitFormBoundaryXYZ

------WebKitFormBoundaryXYZ
Content-Disposition: form-data; name="file"; filename="configuration.md"
Content-Type: application/octet-stream

<文件二进制内容>
------WebKitFormBoundaryXYZ--
```

**关于 MIME：** OpenAPI 里 `file` 声明 `contentMediaType: application/octet-stream`，那只是"二进制文件"的占位声明。参数说明原文是 `file` (UploadFile): The file to be uploaded. **It must have an allowed extension.** —— 校验只看扩展名，part 上的 `Content-Type` 写什么都不影响。reqwest 侧同理：`Part::file()` 只设 `filename`，不设 `Content-Type`；`mime_str()` 是可选的（`reqwest-0.13.5/src/async_impl/multipart.rs:306`）。

```rust
// 不写，LightRAG 一样收
let part = Part::file(path).await?.file_name("x.md");

// 写了，纯属锦上添花，服务端不看
let part = Part::file(path).await?.file_name("x.md").mime_str("text/markdown")?;
```

**注意这不等于 MIME 到处都能省。** MinerU 那条路必须精确 —— `src/file_mime.rs` 的表取自 MinerU 自身的 `MIME_TYPE_BY_EXTENSION`（`mineru/filetypes.py`），标准库对 `mhtml` / `mht` / `rst` / `tex` / `ofd` / `shtml` 给的值与服务端不一致。

**`file_name()` 也要显式设置。** LightRAG 用 filename 判重名，`Part::file()` 默认带完整路径 `/home/.../configuration.md`，会导致 basename 取错。

### 3.2 小文本文件

文件：`/tmp/opencode/opencode-upload-test-20261017.txt`（772 B）

响应 `HTTP 200`，耗时 **35 ms**：

```json
{
  "status": "success",
  "message": "File 'opencode-upload-test-20261017.txt' uploaded successfully. Processing will continue in background.",
  "track_id": "upload_20261017_002419_cabb2f15"
}
```

轮询 `GET /documents/track_status/upload_20261017_002419_cabb2f15` → `"status_summary": {"processed": 1}`：

```json
{
  "track_id": "upload_20261017_002419_cabb2f15",
  "documents": [{
    "id": "doc-ca1682790e657199eab1b22d3e556f98",
    "status": "processed",
    "chunks_count": 1,
    "error_msg": null,
    "content_length": 549,
    "file_path": "opencode-upload-test-20261017.txt",
    "created_at": "2026-10-16T16:24:19.221251+00:00",
    "updated_at": "2026-10-16T16:25:23.969476+00:00",
    "metadata": {
      "source_file": "opencode-upload-test-20261017.txt",
      "parse_engine": "legacy",
      "parse_format": "raw",
      "chunk_method": "recursive_character",
      "chunk_opts": "size=1200, seps=['\\n\\n', '\\n', '。', ...], overlap=100",
      "kg_write_state": "graph_mutation_started",
      "analyzing_stage_skipped": true,
      "parse_start_time": 1792167859,
      "process_end_time": 1792167923
    }
  }],
  "total_count": 1,
  "status_summary": {"processed": 1}
}
```

- 接收耗时 35 ms，端到端（写入到 processed）约 **64 s**。
- 抽取结果：**1 个 chunk → 6 实体 + 7 关系**。
- `.txt` 走 `legacy/raw` 解析器，内容无表格和图片，`analyzing_stage_skipped: true`，跳过多模态分析。

### 3.3 真实 Markdown 文档（取自本仓库 docs）

文件：`docs/configuration.md`（8593 B），响应 `HTTP 200`，耗时 **31 ms**：

```json
{
  "status": "success",
  "message": "File 'configuration.md' uploaded successfully. Processing will continue in background.",
  "track_id": "upload_20261017_002603_f93bcf1d"
}
```

`GET /documents/track_status/upload_20261017_002603_f93bcf1d`（处理中的快照）：

```json
{
  "id": "doc-a66b35d20295cb764719ac8bd35837ec",
  "status": "processing",
  "chunks_count": 6,
  "content_length": 8869,
  "file_path": "configuration.md",
  "metadata": {
    "parse_engine": "native",
    "parse_format": "lightrag",
    "chunk_method": "paragraph_semantic",
    "chunk_opts": "size=2000, overlap=100",
    "mm_chunks": 4,
    "kg_write_state": "pre_graph",
    "process_options": "teP"
  },
  "total_count": 1,
  "status_summary": {"processing": 1}
}
```

与 3.2 的差别：`.md` 走 `native/lightrag` 解析器，按段落语义切成 6 个 chunk，其中 4 个是表格块，因而触发了表格多模态分析。`GET /documents/pipeline_status` 里的逐块抽取进度：

```text
Parsing (native): doc-a66b35d20295cb764719ac8bd35837ec
Analyzing multimodal: doc-a66b35d20295cb764719ac8bd35837ec
Analyzing  table/tb-...-0003: ok
Analyzing  table/tb-...-0002: ok
Analyzing  table/tb-...-0001: ok
Analyzing  table/tb-...-0004: ok
Extracting stage 1/1: configuration.md
Chunk 1 of 6 extracted 16 Ent + 31 Rel doc-...-mm-table-000
Chunk 2 of 6 extracted 24 Ent + 45 Rel doc-...-mm-table-001
Chunk 3 of 6 extracted 36 Ent + 35 Rel doc-...-chunk-000
Chunk 4 of 6 extracted 30 Ent + 64 Rel doc-...-mm-table-003
Chunk 5 of 6 extracted 19 Ent + 38 Rel doc-...-mm-table-002
```

最终状态：

```json
{
  "status_summary": {"processed": 1},
  "documents": [{
    "id": "doc-a66b35d20295cb764719ac8bd35837ec",
    "status": "processed",
    "chunks_count": 6,
    "error_msg": null,
    "metadata": {"parse_engine": "native", "parse_format": "lightrag", "mm_chunks": 4, "kg_write_state": "graph_mutation_started"}
  }]
}
```

- 端到端耗时 **728 s（约 12 分钟）**，上传响应本身仍是 31 ms。
- 抽取结果：**6 个 chunk → 120 实体 + 237 关系**。
- 合并阶段日志：

```text
Phase 1: Processing 120 entities from doc-... (async: 8)
Merged: `Coordinator` | 1+4
Merged: `Correctness` | 1+1
Merged: `Performance` | 1+1
Merged: `Availability` | 1+1
Merged: `Availability Mode` | 1+1
Phase 2: Processing 237 relations from doc-... (async: 8)
Completed merging: 120 entities, 0 extra entities, 237 relations
Completed processing file 1/1: configuration.md
Enqueued document processing pipeline stopped
```

`0 extra entities` 说明 6 个 chunk 之间存在大量重复实体，合并阶段做了去重。文档里的表格 YAML 键名（`Coordinator`、`Correctness`、`Performance`、`Availability`）被正确识别成了实体。

## 4. 异常路径测试

| # | 场景 | 状态码 | 返回体 |
| --- | --- | --- | --- |
| T1 | 同名文件重复上传 | `409` | `{"detail":"Document storage already contains 'opencode-upload-test-20261017.txt' (Status: processed). Delete the existing record before re-uploading."}` |
| T2 | 不支持的后缀 `.exe` | `400` | `{"detail":"Unsupported file type. Supported types: ('.bat', '.c', ..., '.xlsx', '.xml', '.yaml', '.yml')"}` |
| T3 | 缺少 `file` 字段 | `422` | `{"detail":[{"type":"missing","loc":["body","file"],"msg":"Field required"}]}` |
| T4 | 不带 token | `401` | `{"detail":"No credentials provided. Please login."}` |
| T5 | 空文件、无扩展名 | `400` | `{"detail":"Unsupported file type. Supported types: (...)"}` |

全部与 OpenAPI 文档描述一致。T1 尤其符合文档说明：同名字段在写文件之前就同步检测，直接返回 409 要求先删掉已有记录，不再返回 200 + `status="duplicated"`。

服务端支持 41 种扩展名，含 `.txt` `.md` `.pdf` `.docx` `.xlsx` `.pptx` `.epub` `.html` `.json` `.csv` 等。

## 5. 删除文档

### 5.1 请求

路径是 `DELETE /documents/delete_document`，**DELETE 带 JSON body**（`DeleteDocRequest`），只有 `doc_ids` 必填。

```bash
curl -X DELETE http://192.168.2.7:9621/documents/delete_document \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
        "doc_ids": ["doc-ca1682790e657199eab1b22d3e556f98"],
        "delete_file": true,
        "delete_llm_cache": false
      }'
```

Rust 侧：

```rust
let resp = client
    .delete(format!("{}/documents/delete_document", url))
    .bearer_auth(token)
    .json(&DeleteReq { doc_ids, delete_file: true, delete_llm_cache: false })
    .send()
    .await?;
```

| 字段 | 类型 | 默认 | 作用 |
| --- | --- | --- | --- |
| `doc_ids` | `string[]` | 必填 | `doc-` 开头的 ID 数组，可批量 |
| `delete_file` | `bool` | `false` | 是否连 input 目录里的源文件一起删 |
| `delete_llm_cache` | `bool` | `false` | 是否清掉该文档的 LLM 抽取缓存 |

### 5.2 delete_llm_cache 与同名重传

**`delete_llm_cache` 跟"能不能重传同名"无关。** 决定同名能否重传的是 `delete_file`：那个同步 409 查的是 doc_status 记录 + input 目录文件名两处，`delete_file: true` 才清掉后者。缓存在判重那一步压根不查。

`delete_llm_cache` 管的是**速度**，不是正确性。OpenAPI 原文：

> Deletes specific documents and all their associated data... **When requested, cached LLM extraction responses are removed after graph deletion/rebuild completes.**

注意 "after ... completes" —— 缓存清理排在图谱删除/重建**之后**，是异步链上的一环，不影响删除本身。

| 场景 | delete_llm_cache | 理由 |
| --- | --- | --- |
| 文件内容没改，只是删错了想重来 | `false` | 缓存命中，跳过全部 LLM 抽取 |
| 内容改了要覆盖 | `true` | cache key 变了，留着纯占空间 |
| 怀疑之前抽取出错（实体/关系明显不对） | `true` | 否则重传会复用同样错误的抽取结果 |

第一条收益可观：`docs/configuration.md`（8593 B）端到端 **728 s**，走的就是 4 个表格块的 LLM 抽取。留着缓存重传同内容能省掉绝大部分耗时；`true` 等于把这 12 分钟再赔一遍。

### 5.3 如何确认删除完成

**只看一件事：文档列表里 doc_id 消失了。** 轮询 `POST /documents/paginated` 直到查不到该 doc。

```text
1. DELETE /documents/delete_document   {delete_file: true, delete_llm_cache: false}
2. 判 status == "deletion_started"（busy / not_allowed 说明什么都没删，重试）
3. 轮询 POST /documents/paginated，确认 doc_id 不在列表里
```

该端点的响应结构和边界条件见 §5.7。

**不要用 `pipeline_status` 判定删除完成。** `destructive_busy` / `busy` / `scanning` / `pending_enqueues` 都是**全局共享**标志：

- 并发删除、`DELETE /documents`（Clear Documents）、`/documents/scan` 都会置位 —— 分不清标志背后是**谁**的任务在跑，可能等错对象
- 更糟的是时序：自己的后台任务还没启动的瞬间就可能读到 `false`，从而提前判定成功，随后文档才被删掉
- 它只覆盖本服务的写者。别的客户端同时在删同一个库，这个标志对你无意义

文档列表没有这种歧义 —— 它查的就是目标本身。代价是每轮一个分页请求，比读一个布尔字段重，但这正是可靠性的来源。

`paginated` 的 `page_size` 上限 200，必须翻完所有页（`pagination.has_next`），只看第一页会漏判。

**删除完再上传同名的时序：** DELETE 返回 `deletion_started` 后不要立刻 upload —— 此刻 `destructive_busy` 才刚被置上，立刻 upload 会撞 409（这是服务端拒绝，不是判定问题）。等 `paginated` 里 doc_id 消失后再传。

`track_status/{track_id}` 是更轻的方案（不分页、97 B vs 全库分页），**2026-10-18 已实测**：删除完成后 `documents` 变空数组。但空数组无法区分"已删除"和"track_id 从不存在"，所以必须**先建立基线**再等它变空 —— 详见 §9.1。

两种方案对比：

| | `track_status/{track_id}` | `paginated` |
| --- | --- | --- |
| 前提 | 上传时持久化 track_id | 无 |
| 单次响应 | 97 B ~ 1.2 KB | 每条含 `content_summary` + `metadata` |
| 需翻页 | 否 | 是（`page_size` 上限 200） |
| 歧义 | 需基线消除 | 无 |

### 5.4 坑

1. **异步执行**，200 只代表 `status: "deletion_started"`。别拿到 200 就当删完了。
2. **`status: "busy"` 仍是 HTTP 200**，此时**什么都没删**。必须按 §5.2 的表分流。
3. **DELETE 带 body 有些中间层会吞掉。** nginx / 网关 / 某些 HTTP 库可能让 body 到不了服务端，表现是 `422 Field required: doc_ids`。链路上有代理时值得确认。
4. **`delete_file: false` 只删索引和图谱，源文件还在**。再传同名 → `409 Document storage already contains ...`。要重传同名必须 `true`。
5. **不可逆**，图谱关系真没了。

### 5.5 其他相关端点

| 端点 | 用途 |
| --- | --- |
| `DELETE /documents` | 清空全部文档（Clear Documents） |
| `POST /documents/clear_cache` | 清空**全部** LLM 响应缓存，请求体被忽略。别当 per-doc 手段用 |
| `DELETE /graph/entity/delete` | 删单个图实体，与文档级删除不是一回事 |
| `DELETE /graph/relation/delete` | 删单条图关系 |

### 5.6 pipeline_status 的 schema 陷阱

`PipelineStatusResponse` 的 schema 是 `additionalProperties: true`。**实测（2026-10-18）顶层有 19 个键**：

```text
batchs, busy, cancellation_detail, cancellation_reason, cancellation_requested,
cur_batch, destructive_busy, docs, history_messages, job_name, job_start,
latest_message, pending_enqueues, recovery_kind, recovery_message,
recovery_required, scanning, scanning_exclusive, update_status
```

其中 `destructive_busy` / `scanning_exclusive` / `pending_enqueues` **不在 schema 的 `properties` 里但实际存在**（upload / delete 的描述引用了它们）。解析时不要按 schema 写死字段列表。

这个端点适合**人看**和排查 409，不适合做程序化的完成判定 —— 理由见 §5.3。

### 5.7 `paginated` 响应与边界（2026-10-18 实测）

删除确认（§5.3）依赖的查询端点。`POST /documents/paginated`，请求体 `DocumentsRequest`：

```json
{"page": 1, "page_size": 200}
```

完整响应（3 条文档的真实数据）：

```json
{
  "documents": [
    {
      "id": "doc-a66b35d20295cb764719ac8bd35837ec",
      "content_summary": "# Configuration\n\nEvery flag of the long-running commands, the YAML key that mirrors it, its\ndefault, and its scope. `--help` lists the same flags; ...",
      "content_length": 8869,
      "status": "processed",
      "created_at": "2026-10-16T16:26:04.002629+00:00",
      "updated_at": "2026-10-16T16:38:12.633572+00:00",
      "track_id": "upload_20261017_002603_f93bcf1d",
      "chunks_count": 6,
      "error_msg": null,
      "metadata": {
        "mm_chunks": 4,
        "chunk_opts": "size=2000, overlap=100",
        "source_file": "configuration.md",
        "chunk_method": "paragraph_semantic",
        "parse_engine": "native",
        "parse_format": "lightrag",
        "kg_write_state": "graph_mutation_started",
        "parse_end_time": 1792167964,
        "process_options": "teP",
        "parse_start_time": 1792167964,
        "process_end_time": 1792168692,
        "analyzing_end_time": 1792167988,
        "process_start_time": 1792167988,
        "analyzing_start_time": 1792167964
      },
      "file_path": "configuration.md"
    }
  ],
  "pagination": {
    "page": 1,
    "page_size": 200,
    "total_count": 3,
    "total_pages": 1,
    "has_next": false,
    "has_prev": false
  },
  "status_counts": {
    "processed": 3,
    "all": 3
  }
}
```

单条约 1.1 KB（3 条共 3382 B）。**重**在两处：`content_summary` 是必返字段且约 200 字，`metadata` 是自由键值 map（这里 15 个键）。只为了确认一个 id 存在却要拉这些，是 `paginated` 方案的根本开销。

`status_counts` 是**全库**统计，不受当前页和 `status_filters` 影响（实测传 `status_filters:["processed"]`，`total_count` 仍是 3）。

边界实测：

| 请求体 | 状态码 | 结果 |
| --- | --- | --- |
| `{"page":1,"page_size":9}` | `422` | `greater_than_equal` — 下限 10 |
| `{"page":1,"page_size":201}` | `422` | `less_than_equal` — 上限 200 |
| `{"page":0,"page_size":50}` | `422` | `greater_than_equal` — page 下限 1 |
| `{"page":2,"page_size":200}`（越界页） | `200` | `documents: []`，但 `total_count:3`、`total_pages:1` |
| `{"page_size":50}`（省略 page） | `200` | `page` 和 `page_size` 都有默认值 |
| `sort_field: "bogus"` | `422` | `literal_error`，只接受 4 个枚举值 |
| `status_filters: ["不存在的状态"]` | `422` | `enum`，只接受 7 个小写状态名 |

两个容易踩的点：

1. **越界页返回 `200` + 空 `documents`**，和"库里没有文档"形状一样。别用 `documents.is_empty()` 判断翻页结束，要用 `pagination.has_next`。
2. **`total_pages` 不随请求的 `page` 变化**（越界时仍是 1）。别拿 `page / page_size` 自己算总页数，信 `has_next`。

**没有**按 `doc_id` 或 `file_path` 过滤的选项 —— `DocumentsRequest` 可筛的只有 `status_filter` / `status_filters` / `page` / `page_size` / `sort_field` / `sort_direction`。所以"doc X 还在吗"只能扫。

`src/lightrag_requests.rs` / `src/lightrag_model.rs` 已按本文修正，以下是改掉的东西。

### 6.1 已修

| 位置 | 原问题 | 现状 |
| --- | --- | --- |
| `lightrag_requests.rs` `_upload` | `resp_json.status == "sucess"` 拼写错误，**每次成功上传都走进 else 分支返回 Err** | 改为 `"success"` |
| `lightrag_requests.rs` `_upload` | `.file_name(file_path)` 传完整路径，服务端拿 filename 判同名，整条路径会污染判重和落盘目录 | 改为取 `Path::file_name()` 的 basename |
| `lightrag_requests.rs` `_track` | URL 是 `/documents/track/{id}`，实际端点是 `/documents/track_status/{id}` → 404 | 已改正 |
| `lightrag_requests.rs` `_track` | 轮询一次就返回，而上传 31 ms 返回、后台要 64 s~728 s，一次查询必然拿不到终态 | 改为按 `POLL_INTERVAL` 轮询到终态，`TRACK_TIMEOUT` 900 s |
| `lightrag_requests.rs` `delete_document` | 任何 2xx 都 `Ok(())`，把 `busy`（**什么都没删**）当成功 | 判 `status`：`busy` / `not_allowed` 返回 Err |
| `lightrag_requests.rs` `delete_document` | 删除是异步的，返回即走，没等后台删完 | 轮询 `paginated` 到 doc_id 从列表消失才返回 Ok |

model 侧：

| 位置 | 原问题 | 现状 |
| --- | --- | --- |
| `lightrag_model.rs` `TrackResponse` | 顶层有 `id` / `status`，实际响应是 `documents[]` 信封结构 → 反序列化必然失败 | 改为 `TrackResponse { track_id, documents[], total_count, status_summary }` |
| `lightrag_model.rs` `StatusCodeResponse` | 只有 `processing` 一个字段，实际键是 `processed` / `failed` 等 → missing field | 改为 `HashMap<String, u64>`，比对时忽略大小写（OpenAPI 示例给 `"PROCESSED"`，实测给 `"processed"`） |
| `lightrag_model.rs` `MetadataResponse` | 定长 7 字段且无 default，处理中/终态字段集合不同 → 任一阶段都反序列化失败 | 改为 `HashMap<String, serde_json::Value>` |

新增 model：`DocStatusResponse`、`DeleteDocByIdResponse`、`PaginatedDocsResponse`、`PaginationInfo`、`DocumentsRequest`。

`PipelineStatusResponse` 加过又删了 —— 判定删除完成不需要它，理由见 §5.3。

### 6.2 同名冲突自愈放在 `upload_file` 内部

**为什么不放调用方：** 调用方手上只有本地路径，判不出 409 说的是哪个 `doc_id`；而 `doc_id` 必须回服务端按文件名反查（`RAGRequest` 不持有 DB 连接，拿不到调用方存的 `rag_id`）。自愈逻辑放在外面，调用方就只能靠「无条件先删」或「吞掉 409」来绕，两个都不对。

`_upload` 因此拆出 `UploadError`：

| 变体 | 含义 | 处理 |
| --- | --- | --- |
| `Conflict(detail)` | `409`，detail 保留原文以区分冲突来源（文档存储 / input 目录） | 清理后**重试一次** |
| `Fatal(msg)` | 400 / 401 / 5xx / 网络错误 | 原样上抛 |

只对 `Conflict` 重试。对 400/401 也重试会把真实错误掩盖成一次莫名其妙的失败。**只重试一次** —— 删完再传还是 409 说明有别的成因，再循环只是把一次失败拖成无数次请求。

`remove_conflicting` 的三种结果：

| `find_doc_ids_by_filename` 返回 | 动作 |
| --- | --- |
| 恰好 1 个 | `delete_document` 后重传 |
| 0 个 | 报错。冲突多半来自 input 目录里的残留文件（无索引记录），`delete_document` 清不掉，需要人工介入 |
| > 1 个 | 报错并列出 id。文件名是唯一键，出现多个说明有异常，盲删有误伤风险 |

文件名比对用 `canonical_basename()`：LightRAG 判同名时会剥掉 parser hint（`abc.docx` 与 `abc.[native].docx` 同指一条记录）。实现里**只剥实测见过的 `[native]` 和 `[legacy]`** 两种（§3.2 / §3.3 的 parse_engine），没见过的 hint 不猜 —— 宁可匹配不上让人看一眼，也不要按错名字删掉别人的文档。

### 6.3 `is_finished()` 里那个 `!is_empty()`

写这个测试时当场撞出来的：**上传返回 track_id 的瞬间，`doc_status` 里的记录未必已经注册上，第一次 `track_status` 查询会拿到 `documents: []`**，随后同一个 track_id 就正常了。

空数组上 `all()` 恒为 `true`，于是 `is_finished()` 会把「还没注册」直接判成「处理完成」，紧接着 `first_doc_id()` 返回 `None`，报「没有返回任何 doc_id」。修法是 `!self.documents.is_empty() && all(...)`。

这个空数组和「文档已被删除」的响应**逐字节相同**（§9.1 的 97 B），所以既不能当完成信号，也不能当失败信号，只能继续等。

### 6.4 端到端测试

`lightrag_requests.rs` 内有 `live_upload_conflict`，打真实服务，默认 `--ignore`。覆盖：首传 → 同名重传（走冲突分支）→ 改内容重传（仍走冲突分支）→ 删除并确认消失。

```sh
RAG_URL=... RAG_USER=... RAG_PASS=... \
cargo test live_upload_conflict -- --nocapture --ignored
```

约 90 s（三轮上传各等一次后台抽取）。

实测结果（2026-10-18）：

```text
第一次上传 -> doc-7499fdee92a749e270800a40e067fbeb
[冲突] rag_sync_live_conflict_probe.txt 已被 doc-7499fdee92a749e270800a40e067fbeb 占用，先删除再重传
重传同名 -> doc-7499fdee92a749e270800a40e067fbeb
[冲突] rag_sync_live_conflict_probe.txt 已被 doc-7499fdee92a749e270800a40e067fbeb 占用，先删除再重传
改内容重传 -> doc-7499fdee92a749e270800a40e067fbeb
全部通过
```

三轮的 doc_id 完全相同 —— 这正是 §9.0 的结论：doc_id 由文件名派生，与内容无关。

### 6.5 遗留

- `lightrag_requests.rs:1` 的 `use tokio::stream;` 是无用 import。
- `_track` / `upload_file` / `delete_document` 在生产路径上还只有 `sync_folder.rs` 间接用到 `upload_file`，其余带 `dead_code` 警告。


## 7. 结论

- 鉴权走 `/login` 换 Bearer token，`grant_type=password` 必填，body 是 form-urlencoded 纯文本，token 字段名 `access_token`。
- 上传只认**扩展名**，part 的 MIME 可以不设；`file_name()` 必须显式设，否则 basename 取错影响判重。
- 上传本身极快（31–35 ms），文件落盘后立即返回 `track_id`；解析和图谱抽取是异步的，必须靠 `track_status` 轮询。
- 文件名重复是**同步** 409，内容重复是**异步** 判定，最终以 `FAILED` 收场，`error_msg` 里带 `Content already exists. Original doc_id: xxx`。
- 后台处理耗时和内容强相关，不是线性于文件大小：549 B 纯文本 64 s，8869 B Markdown 因含 4 个表格、抽出 120 实体 + 237 关系，花了 728 s。**绝对不要在上传请求上等结果**。
- 删除走 `DELETE /documents/delete_document` 带 JSON body，异步执行；`delete_file` 决定能否重传同名，`delete_llm_cache` 只影响速度。
- 确认删除完成**只能靠文档列表**（轮询 `paginated` 到 doc_id 消失），不能用 `pipeline_status` —— 那些标志是全局共享的，分不清是谁的任务。
- 状态字段在 `documents[].status`，汇总是 `status_summary`，**顶层没有 `status`**。按顶层解析会静默拿到空值，这是最容易踩的坑。

## 8. 测试产生的副作用

**2026-10-18 已清理。** 本次测试向知识库写入的 2 个文档均已删除，库中现存的 3 篇全部是测试前的文档：

| 文件 | doc_id | 来源 |
| --- | --- | --- |
| `configuration.md` | `doc-a66b35d20295cb764719ac8bd35837ec` | 测试上传（`docs/configuration.md`） |
| `experiments.md` | `doc-6645ec8fc76c37978e3ea87c359a975c` | 测试前已存在（scan 导入） |
| `互联网链路安全的区块链加固_论文点评.md` | `doc-9c8c165e3a6eceae96a29b4c9c424095` | 测试前已存在 |

已删除的：

| 文件 | doc_id | track_id |
| --- | --- | --- |
| `opencode-upload-test-20261017.txt` | `doc-ca1682790e657199eab1b22d3e556f98` | `upload_20261017_002419_cabb2f15` |
| `delprobe.txt`（计时探针） | `doc-10646ae5473fd024d5aa5df367c35f0e` | `upload_20261018_114952_3c9660a9` |
| `ctl.txt`（JSON 转义探针） | `doc-1c258bc3fc3ee8d503f3ad4a7c04711c` | `upload_20261018_115034_3b687b65` |

三个探针文件都用 `delete_file: true` 删除，input 目录里的源文件已一并清除。

凭据取自 `config.yml`（已被 `.gitignore:5` 忽略，未被 git 跟踪）。

## 9. 未验证项

本节列出**没有实测依据**的判断。写代码时不要把这些当既定事实。

### 9.0 `doc_id` 的派生规则 —— 已实测（2026-10-18）

**`doc_id` = `"doc-" + md5(文件名)`**，不含路径、不含扩展名处理、不含内容。对 4 个样本逐一核对，4/4 精确命中：

| 文件名 | md5(文件名) | 观测到的 doc_id |
| --- | --- | --- |
| `configuration.md` | `a66b35d20295cb764719ac8bd35837ec` | `doc-a66b35d20295cb764719ac8bd35837ec` ✓ |
| `experiments.md` | `6645ec8fc76c37978e3ea87c359a975c` | `doc-6645ec8fc76c37978e3ea87c359a975c` ✓ |
| `互联网链路安全的区块链加固_论文点评.md` | `9c8c165e3a6eceae96a29b4c9c424095` | `doc-9c8c165e3a6eceae96a29b4c9c424095` ✓ |
| `rag_sync_live_conflict_probe.txt` | `7499fdee92a749e270800a40e067fbeb` | `doc-7499fdee92a749e270800a40e067fbeb` ✓ |

推论：

1. **doc_id 是客户端可预知的**，不必靠上传响应回传。rag-sync 的 `rag_id` 落库时可以自己算，省一次往返。
2. **改内容不影响 doc_id** —— 实测把探针文件内容改掉后重传，doc_id 一模一样。所以「doc_id 没变」**不能**用来判断内容是否被更新过。
3. **删掉再传同名文件，doc_id 会原样回来**。这给 §9.1 的删除确认加了一条约束：轮询到 doc_id 消失之后，如果同名文件被重新上传，doc_id 会**再次出现同一个值**。「消失过」不等于「不会再回来」。
4. 与文件名是唯一键一致 —— 这就是 409 同步判重的根据，也是 §6.2 自愈逻辑能按文件名反查的前提。

**这是逆向观察到的实现细节，不是服务端承诺的契约。** 依赖它去预计算 doc_id 之前，先在自己的库上核对一遍（一条 `md5` 的事）。`remove_conflicting` 仍然走 `paginated` 扫描而不直接用 md5 算，就是因为扫描是权威的、还顺带处理了 parser hint 归一。

注意区分：内容重复检测（§7 的 `Content already exists. Original doc_id: xxx`）是**异步**的，它报的 `Original doc_id` 指向的是**另一个文件名**下内容相同的文档 —— 那种情况 doc_id 自然不同，因为 md5 前像不同。

### 9.1 删除后 `track_status` 的行为 —— 已实测（2026-10-18）

凭据取自 `config.yml`（该文件在 `.gitignore` 里，未被跟踪）。

| 输入 | 状态码 | 响应 | 字节 |
| --- | --- | --- | --- |
| 删除前，已存在的 track | `200` | `{"track_id":...,"documents":[{...}],"total_count":1,"status_summary":{"processed":1}}` | 1198 |
| 删完后，同一 track | `200` | `{"track_id":...,"documents":[],"total_count":0,"status_summary":{}}` | **97** |
| **从未存在过的 track** | `200` | `{"track_id":...,"documents":[],"total_count":0,"status_summary":{}}` | **97** |
| 空 track_id（路径尾部 `/`） | `404` | `{"detail":"Not Found"}` | — |

三个必须知道的点：

1. **响应从 1198 B 缩到 97 B**，且不分页。这是 `paginated` 方案（每条返回完整 `DocStatusResponse`，含 `content_summary` 和自由键值的 `metadata`）比不了的轻量级。

2. **删除完成后不是立刻生效。** 0.3 s 间隔密集轮询实测：

   ```text
   +0.07s  documents=1  bytes=942   ← 还在
   +0.42s  documents=0  bytes=97    ← 消失
   ```

   拿到 `deletion_started` 后第一次查询**仍然能看到文档**。所以必须轮询，查一次不算。

3. **空数组不能单独作为删除成功的证据。** 已删除的 track 和从未存在的 track 返回**逐字节相同**的 97 B 响应。`documents: []` 只说明"这个 track 下现在没有文档"，不说明"曾经有过、被你删掉了"。

   还有一个反向的坑：**上传后立即查询也会拿到 `documents: []`** —— `doc_status` 记录尚未注册（实测撞到过，见 §6.3）。所以空数组既可能是"还没开始"，也可能是"已经删了"，只能靠前后基线区分。

   正确用法是**先建立基线，再等它变空**：删除前先确认该 track 下能看到目标 doc_id，之后轮询到空，才能确定是这次删除生效。

   ```text
   1. GET  track_status/{track}          → 断言 documents[] 里有目标 doc_id   ← 基线
   2. DELETE /documents/delete_document  → 断言 status == "deletion_started"
   3. GET  track_status/{track} 轮询     → documents[] 不再含该 doc_id 即完成
   ```

   没有第 1 步，track_id 写错、被服务端淘汰、文档本来就属于别的 track，都会静默判成"删除成功"。

   另外注意 §9.0：同名文件重传后 **doc_id 会原样回来**。"消失过"不等于"不会再回来"。

`track_status` 不存在的 track_id 返回 `200` + 空数组而不是 404，所以它**不是**一个能用来探测 track_id 是否有效的端点。

### 9.2 LLM 缓存的 key 构成

是否严格按内容哈希 / chunk 内容命中，无法确认。这台机器上没有服务端源码（`~/projects`、`/opt`、`/srv` 下均无 lightrag 源码），读不到这个定制版 1.5.7 的实现。

5.2 节"同内容重传可命中缓存"的结论基于 LightRAG 的通用机制推断，**未经实测**。验证：同一个文件删了重传一次，比对两次的 `process_end_time`，以及 pipeline 日志里是否有 cache hit 字样。

### 9.3 DELETE body 被中间层吞掉

5.4 第 3 条属于已知的通用风险，本机链路上**未复现**。当前 rag-sync 直连 `192.168.2.7:9621`，中间没有代理层；将来若加 nginx / 网关 / 换 HTTP 库，需要重新确认。

### 9.4 `/documents/{doc_id}` 路径

`POST /documents/upload` 的 OpenAPI 描述里写着"Clients must delete the existing document (``DELETE /documents/{doc_id}``)"，但实测 `/openapi.json` 的路径表里**只有** `/documents/delete_document` 和 `/documents/clear_cache`，没有 `/documents/{doc_id}`。以实际路径表为准；描述与实现不符这点本身尚未追查原因。
