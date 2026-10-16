# LightRAG `/documents/upload` 接口测试报告

测试时间：2026-10-17
目标服务：`http://192.168.2.7:9621`（LightRAG Server，`core_version 1.5.7`，`api_version 0344`，WebUI 标题 "cmach_socket Graph KB"）
被测端点：`POST /documents/upload` — *Upload To Input Dir*

## 1. 接口契约

以下来自服务端 `/openapi.json`：

| 项 | 值 |
| --- | --- |
| 方法与路径 | `POST /documents/upload` |
| 请求体 | `multipart/form-data`，必填字段 `file`（`application/octet-stream`） |
| 认证 | `Authorization: Bearer <token>`（`OAuth2PasswordBearer`，tokenUrl 为 `/login`） |
| 成功响应 | `200` + `InsertResponse`：`{status, message, track_id}`，`status` 枚举 `success / partial_success / failure` |
| 其他响应 | `422` 参数校验失败、`400` 不支持的文件类型、`401` 未认证、`409` 同名冲突或独占写状态 |
| 大小限制 | `MAX_UPLOAD_SIZE`，默认 100 MB，超限返回 `413` |

处理是**异步**的：上传只负责把文件写进 input 目录并立刻返回 `track_id`，真正的解析与图谱抽取由后台流水线完成，需要轮询 `GET /documents/track_status/{track_id}` 拿最终结果。

## 2. 认证

```
POST /login
Content-Type: application/x-www-form-urlencoded

grant_type=password&username=<user>&password=<pwd>
```

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

注意 `expires_in` 字段不存在，看不出有过期时间。

未带 token 访问受保护端点：

```
GET /documents/supported_file_types   → HTTP 401
{"detail":"No credentials provided. Please login."}
```

`GET /auth-status`（免鉴权）确认该服务开启了鉴权：

```json
{"auth_configured":true,"auth_mode":"enabled","core_version":"1.5.7","api_version":"0344"}
```

## 3. 正常上传测试

### 3.1 小文本文件

文件：`/tmp/opencode/opencode-upload-test-20261017.txt`（772 B）

```
POST /documents/upload
-F "file=@/tmp/opencode/opencode-upload-test-20261017.txt"
```

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

### 3.2 真实 Markdown 文档（取自本仓库 docs）

文件：`docs/configuration.md`（8593 B）

```
POST /documents/upload
-F "file=@/home/cmach_socket/projects/rag-sync/docs/configuration.md"
```

响应 `HTTP 200`，耗时 **31 ms**：

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

与 3.1 的差别：`.md` 走 `native/lightrag` 解析器，按段落语义切成 6 个 chunk，其中 4 个是表格块，因而触发了表格多模态分析。`GET /documents/pipeline_status` 里的逐块抽取进度：

```
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

最终状态（继续轮询直到结束）：

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

```
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

## 5. 结论

接口工作正常，`/documents/upload` 整条链路可用：

- 鉴权走 `/login` 换 Bearer token，`auth_mode: enabled` 时所有 `/documents/*` 都需要它。
- 上传本身极快（31–35 ms），文件落盘后立即返回 `track_id`；解析和图谱抽取是异步的，必须靠 `track_status` 轮询。
- 文件名重复是**同步** 409，内容重复是**异步** 判定，最终会以 `FAILED` 收场，`error_msg` 里带 `Content already exists. Original doc_id: xxx`。
- 后台处理耗时和内容强相关，不是线性于文件大小：549 B 的纯文本 64 s，而 8869 B 的 Markdown 因为含 4 个表格、抽出 120 实体 + 237 关系，花了 728 s。**绝对不要在上传请求上等结果**，`track_id` 拿到就走。
- 状态字段在 `documents[].status`，汇总是 `status_summary`，**顶层没有 `status`**。按顶层解析会静默拿到空值，这是最容易踩的坑。

## 6. 测试产生的副作用

本次测试向知识库写入了 2 个文档，未做清理：

| 文件 | doc_id | track_id |
| --- | --- | --- |
| `opencode-upload-test-20261017.txt` | `doc-ca1682790e657199eab1b22d3e556f98` | `upload_20261017_002419_cabb2f15` |
| `configuration.md` | `doc-a66b35d20295cb764719ac8bd35837ec` | `upload_20261017_002603_f93bcf1d` |

清理命令（`DELETE /documents/delete_document`，请求体为 `DeleteDocRequest`；`delete_file` 默认 false，为 true 时会同时删掉 input 目录里的文件）：

```
DELETE /documents/delete_document
{
  "doc_ids": ["doc-ca1682790e657199eab1b22d3e556f98", "doc-a66b35d20295cb764719ac8bd35837ec"],
  "delete_file": true,
  "delete_llm_cache": true
}
```

知识库中另外 2 篇是测试前就有的：`experiments.md`、`互联网链路安全的区块链加固_论文点评.md`。
