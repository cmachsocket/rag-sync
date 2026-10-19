# MinerU `/v1/parse/jobs` 任务接口测试记录

测试时间：2026-10-19
目标服务：`http://127.0.0.1:9777`（`mineru-kit api-server --host 127.0.0.1 --port 9777 --tier standard`，版本 `4.0.12`）
被测端点：`POST /v1/parse/jobs` — *Create Job*、`GET /v1/parse/jobs/{job_id}` — *Get Job*
契约来源：`GET /openapi.json`（`#/components/schemas/CreateJobRequest` / `JobAsyncResponse`），逐条实测核对
测试文件：`test.docx`（14790 B，1 页）、`test.pdf`（758482 B，13 页扫描件，`page_range` 实测 `1-13`）

## 1. 为什么换掉 `/v1/file_parse`

`file_parse` 是异步任务的**同步包装**：客户端挂一条连接干等，`wait_timeout` 上限 `3600`，
超时才回 `202` 转 `GET /v1/tasks/{task_id}`。实测解析耗时（`parse.duration_ms`，tier `standard`）：

| 文件 | 页数 | 解析耗时 |
| --- | --- | --- |
| `test.docx`（纯文本抽取） | 1 | 0.05 ~ 0.6 s |
| `test.pdf`（扫描件，逐页 OCR） | 13 | **83.2 s**（约 6.4 s/页） |

按 6.4 s/页 推算，一份 914 页的文档要 **1.6 小时以上**，超过 `wait_timeout` 的 3600 s：
同步接口要么被掐断、要么占着一条连接空等十几分钟到几小时。
任务接口把等待交给服务端排队，客户端只轮询，代价是每几秒一次 `GET`。

## 2. 请求

```
POST /v1/parse/jobs
Content-Type: application/json

{"files":[{"source":{"type":"file_id","file_id":"file-5e917028473f4e14267b3c77"}}]}
```

`CreateJobRequest` 同为 `additionalProperties: false`，字段多写一个就 `422`：

| 字段 | 必填 | 默认 | 说明 |
| --- | --- | --- | --- |
| `files` | 是（1..100 项） | — | 项内只有 `source` 必填，`page_range` 可选 |
| `tier` | 否 | 服务端配置 | `flash`/`basic`/`standard`/`advanced` |
| `ocr_mode` | 否 | `auto` | `auto`/`txt`/`ocr` |
| `output_formats` | 否 | `["markdown"]` | |
| `callback` | 否 | null | webhook，本机 `features.webhook: false` |

`source` 的四种变体与 `file_parse` 完全一致（`file_id` / `url` / `inline` / `local`）。

响应：**恒 `202`**，与 `GET /v1/parse/jobs/{job_id}` 同构（`JobAsyncResponse`）：

```json
{
  "job_id": "job_238c101477e87831b7c27f65",
  "status": "queued",
  "created_at": "2026-10-19T10:32:00Z",
  "started_at": null,
  "finished_at": null,
  "tier": "standard",
  "output_formats": ["markdown"],
  "access_level": "anonymous",
  "progress": {"completed": 0, "failed": 0, "total": 1},
  "files": [{
    "file_id": "file-5e917028473f4e14267b3c77",
    "name": "test.docx",
    "page_range": "",
    "status": "queued",
    "parse": null,
    "output_files": null,
    "error": null
  }],
  "links": {
    "self": "/v1/parse/jobs/job_238c101477e87831b7c27f65",
    "cancel": "/v1/parse/jobs/job_238c101477e87831b7c27f65"
  }
}
```

## 3. 轮询

```
GET /v1/parse/jobs/{job_id}     → 200 JobAsyncResponse（同上）
                                → 404 {"error":{"code":"job_not_found", ...}}
DELETE /v1/parse/jobs/{job_id}  → 取消（响应 `JobCancelResponse`）
```

终态五种，**只有 `completed` 是成功**：

| 终态 | 含义 |
| --- | --- |
| `completed` | 全部文件成功 |
| `partial` | 有文件失败。单文件任务里等同于失败，必须冒泡 |
| `failed` | 任务失败，看 `files[].error` |
| `canceled` | 被取消（一个 `l`，与 `/v1/tasks` 的 `cancelled` **拼写不同**） |
| — | `queued` / `running` 为非终态，继续轮 |

`files[].status` 另有一套：`queued`/`running`/`completed`/`failed`（没有 `partial`）。

实测时序（`test.pdf`，84 s）：

```
[轮询 1/7200]  状态: running (0/1)      ← 建任务后 1 s
...
[轮询 3/7200]  状态: running (0/1)      ← 前 3 轮 1 s 一问
[轮询 4/7200]  状态: running (0/1)      ← 之后 5 s 一问
...
[轮询 19/7200] 状态: running (0/1)      ← 下一次即终态
```

## 4. 取正文：任务接口**不内联内容**

终态响应：

```json
{
  "job_id": "job_238c101477e87831b7c27f65",
  "status": "completed",
  "started_at": "2026-10-19T10:32:00Z",
  "finished_at": "2026-10-19T10:32:01Z",
  "progress": {"completed": 1, "failed": 0, "total": 1},
  "files": [{
    "file_id": "file-5e917028473f4e14267b3c77",
    "name": "test.docx",
    "page_range": "1",
    "status": "completed",
    "parse": {"model_used": null, "duration_ms": 905, "parser_version": "4.0.12"},
    "output_files": {
      "markdown": {"file_id": "file-cbc0f6f27ba9c8a417359bf1", "bytes": 1565},
      "middle_json": null, "structured_content": null,
      "html": null, "latex": null, "docx": null, "zip": null
    },
    "error": null
  }]
}
```

`JobFileResult` **没有 `content` 字段**（`file_parse` 的 `TaskFileContent` 才有），正文只能二次取：

```
GET /v1/files/{files[0].output_files.markdown.file_id}/content   → markdown 原文（text/plain）
GET /v1/files/{源 file_id}/content                               → 403 feature_requires_api_key
```

失败时 `output_files` 整个是 `null`，`error` 是 `ErrorDetail`：

```json
"error": {"type": "...", "code": null, "message": "...", "param": null}
```

## 5. 与 `/v1/file_parse` 的差异

| | `/v1/file_parse` | `/v1/parse/jobs` |
| --- | --- | --- |
| 提交 | `200` 内联结果 / `202` 转 task | 恒 `202`，拿 `job_id` |
| 状态查询 | `GET /v1/tasks/{task_id}` | `GET /v1/parse/jobs/{job_id}` |
| 终态 | `completed`/`failed`/`cancelled` | `completed`/`partial`/`failed`/`canceled` |
| 正文 | 同步 `200` 时内联 `content.markdown` | **从不内联**，只能按产物 id 取 |
| 卡住时 | 占着连接直到 `wait_timeout` | 只占轮询请求，服务端自己排队 |

## 6. rag-sync 的实现要点

`src/mineru_requests.rs::parse`：

- 建任务 → 判 `is_terminal` → 不是终态才 `poll`（终态直接落进来就省一轮）。
- 轮询节奏：前 3 轮 1 s（小文件 1 s 内就完，固定 5 s 会让一批小文件每个白等 5 s），
  之后 5 s；上限 7200 轮 ≈ 10 h —— 914 页量级要 1.6 小时以上，别把正常解析掐断。
- 非 `completed` 的终态一律当失败冒泡，错误里带 `files[].error` 正文。
- 成功后取 `files[0].output_files.markdown.file_id`，`GET .../content` 拿原文。

覆盖这些分支的用例在 `tests/mineru_test.rs`（`queued_job_polls_until_terminal_then_fetches_artifact`、
`partial_job_is_failure`、`job_canceled_is_failure` 等），全部对 mock 服务端跑、离线。
