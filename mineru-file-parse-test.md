# MinerU `/v1/file_parse` 接口测试记录

测试时间：2026-10-17
目标服务：`http://127.0.0.1:9777`（`mineru-kit api-server --host 127.0.0.1 --port 9777 --tier standard`，版本 `4.0.12`）
被测端点：`POST /v1/file_parse` — *File Parse*
测试文件：`/home/cmach_socket/projects/rag-sync/test.pdf`（758482 B，PDF 1.7，914 页，sha256 `aea7aae5...fa95b3c`）

## 1. 路径纠正

最初按 `/file_parse` 请求，返回 404：

```
GET http://127.0.0.1:9777/file_parse
HTTP/1.1 404 Not Found
{"detail":"Not Found"}
```

所有端点都带 `/v1` 前缀，正确路径为 `POST /v1/file_parse`。

`GET /v1/health` 确认服务状态：

```json
{
  "status": "ok",
  "version": "4.0.12",
  "features": {
    "webhook": false,
    "output_formats": ["markdown", "middle_json", "structured_content", "zip"],
    "sources": ["file_id", "url", "inline"]
  }
}
```

## 2. 接口契约

`file_parse` **不接受文件流**，body 为 JSON，文件来源通过 `source` 的判别字段 `type` 指定，共四种：

| type | 必填字段 | 说明 |
| --- | --- | --- |
| `file_id` | `file_id` | 引用已上传文件（本次使用） |
| `url` | `url` | 远程 URL |
| `inline` | `name`, `data` | 内容直接内联 |
| `local` | `path` | 服务端本地路径 |

查询参数：

| 参数 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `response_format` | `json` \| `zip` | `json` | `zip` 时响应为二进制包 |
| `wait_timeout` | int (1..3600) | `300` | 同步等待秒数；**超时不会取消后台任务** |
| `tier` | `flash`\|`basic`\|`standard`\|`advanced` | 服务端配置 | 本次为 `standard` |
| `ocr_mode` | `auto`\|`txt`\|`ocr` | `auto` | |
| `page_range` | string | 空 | 1-based 闭区间，如 `1-5,8,r3-r1`；`r1` 为末页，`all` 为全部 |
| `output_formats` | string[] | `["markdown"]` | 可选 `markdown`/`middle_json`/`structured_content`/`html`/`latex`/`docx`/`zip` |
| `filename` | string | — | |
| `sha256sum` | `[a-f0-9]{64}` | — | |
| `Content-Length` | header | — | 上限 209715200（200 MB） |

响应：`200` 返回 `TaskResultResponse`，等待超时则 `202` 返回 `TaskStatusResponse`，另有 `422`。因 `file_parse` 是异步任务的同步包装，超时后需用返回的 `task_id` 查 `GET /v1/tasks/{task_id}/result` 兜底。

## 3. 上传文件（三步）

本地文件必须先上传才能得到 `file_id`。

### STEP 1 — 声明上传

需提前算好字节数与 sha256，服务端会校验。

```
POST /v1/uploads
Content-Type: application/json

{"filename":"test.pdf","bytes":758482,"mime_type":"application/pdf","sha256sum":"aea7aae52dc22592e5d7fab2bcb3bd87dd64513a13b5a4354b54b0951fa95b3c"}
```

`200`：

```json
{
  "id": "upload_36838fa63fbb3f71accff2f5",
  "object": "upload",
  "bytes": 758482,
  "created_at": 1792180373,
  "expires_at": 1792183973,
  "filename": "test.pdf",
  "purpose": "parse",
  "mime_type": "application/pdf",
  "sha256sum": "aea7aae52dc22592e5d7fab2bcb3bd87dd64513a13b5a4354b54b0951fa95b3c",
  "status": "pending",
  "upload_url": "http://127.0.0.1:9777/v1/uploads/upload_36838fa63fbb3f71accff2f5/content",
  "upload_method": "PUT",
  "upload_headers": {"Content-Type": "application/pdf"},
  "file": null
}
```

上传有 1 小时有效期（`expires_at`），过期需重新申请。

### STEP 2 — 上传字节

```
PUT /v1/uploads/upload_36838fa63fbb3f71accff2f5/content
Content-Type: application/pdf

<test.pdf 原始字节>
```

`200`，响应体为空。

### STEP 3 — 完成上传

```
POST /v1/uploads/upload_36838fa63fbb3f71accff2f5/complete
```

`200`，`status` 变为 `completed`，并返回 `file_id`：

```json
{
  "id": "upload_36838fa63fbb3f71accff2f5",
  "object": "upload",
  "bytes": 758482,
  "status": "completed",
  "upload_url": null,
  "upload_method": null,
  "file": {
    "id": "file-0daba4e68fe97e9ab87218f9",
    "object": "file",
    "bytes": 758482,
    "created_at": 1792180375,
    "expires_at": 1792183973,
    "filename": "test.pdf",
    "purpose": "parse",
    "sha256sum": "aea7aae52dc22592e5d7fab2bcb3bd87dd64513a13b5a4354b54b0951fa95b3c"
  }
}
```

## 4. 解析

为验证链路先只取前 3 页（全文 914 页）：

```
POST /v1/file_parse?wait_timeout=240&response_format=json
Content-Type: application/json

{
  "files": [
    {
      "source": {"type": "file_id", "file_id": "file-0daba4e68fe97e9ab87218f9"},
      "page_range": "1-3"
    }
  ]
}
```

`200`，耗时 **65.1 s**，响应体 19454 B：

```json
{
  "status": "completed",
  "created_at": 1792180440,
  "started_at": "...",
  "finished_at": "...",
  "tier": "standard",
  "output_formats": ["markdown"],
  "access_level": "...",
  "progress": null,
  "task_id": "job_5f2275d16741485741624f6e",
  "status_url": "...",
  "result_url": "...",
  "links": {"self": "...", "cancel": "..."},
  "files": [
    {
      "name": "test.pdf",
      "status": "completed",
      "page_range": "1-3",
      "error": null,
      "output_files": {
        "markdown": {"file_id": "file-5c1489ebba6abcdad762d6f2", "bytes": 18440},
        "middle_json": null,
        "structured_content": null,
        "html": null,
        "latex": null,
        "docx": null,
        "zip": null
      },
      "content": {"markdown": "<内联的 markdown 字符串，长度 18406>"}
    }
  ]
}
```

`content.markdown` 直接内联返回完整内容，不必再调 `GET /v1/files/{file_id}/content`；`output_files.markdown.file_id` 另给了一个可寻址的 id。

## 5. 返回内容

`content.markdown` 开头（节选）：

```markdown
# Executive summary

Uptime Intelligence report: July 2025

# Uptime Institute Global Data Center Survey 2025

Now in its 15th year, this survey reveals an innovative and resilient industry — but one that is also facing rising costs, worsening power constraints and challenges in meeting the demands for AI. As operators expand and modernize to meet power and density requirements, they need to address availability, efficiency, staffing challenges, supply chain delays and unpredictable technological advances.

**Uptime Intelligence: actionable insight for the digital infrastructure ecosystem.**

To access the entire Uptime Institute Global Data Center Survey 2025 report and Uptime Intelligence on an evaluation basis, please visit [intelligence.uptimeinstitute.com/request-evaluation](https://intelligence.uptimeinstitute.com/request-evaluation?intelligence_request=annual_survey_es_2025)
```

标题层级、段落、加粗、行内链接均正确保留。文件内容为 Uptime Institute Global Data Center Survey 2025 报告。

## 6. 性能

| 范围 | 耗时 |
| --- | --- |
| 3 页 | 65.1 s |

按此推算，914 页全量约需 **5 小时**。若要全量，应显式给足 `wait_timeout`（上限 3600 s），或接受 `202` 后轮询 `GET /v1/tasks/{task_id}/result`。

## 7. DOCX 测试

文件：`test.docx`（14790 B，Microsoft Word 2007+，sha256 `c7c1b584...135095`）

流程与 PDF 相同，仅 `mime_type` 改为
`application/vnd.openxmlformats-officedocument.wordprocessingml.document`。
`POST /v1/uploads` 声明后返回 `upload_270f67f3c686aff2c03d7164`，`PUT` 后 `complete` 得到
`file-ebba75dec4d1b9470a51d095`，服务端回显的 sha256 与本地一致。

解析请求（未指定 `page_range`，全文档）：

```
POST /v1/file_parse?wait_timeout=240&response_format=json

{"files":[{"source":{"type":"file_id","file_id":"file-ebba75dec4d1b9470a51d095"}}]}
```

`200`，耗时 **0.6 s**：

```json
{
  "status": "completed",
  "tier": "standard",
  "output_formats": ["markdown"],
  "task_id": "job_cc4be89aa73efb66a97dd47d",
  "files": [{
    "name": "test.docx",
    "status": "completed",
    "page_range": "1",
    "error": null,
    "output_files": {
      "markdown": {"file_id": "file-6000e05a117052df098b3d11", "bytes": 1565},
      "middle_json": null, "structured_content": null,
      "html": null, "latex": null, "docx": null, "zip": null
    },
    "content": {"markdown": "<长度 749>"}
  }]
}
```

内容为一份计算机图形学作业（DDA / Bresenham 画线算法），标题、加粗、中文与全角标点均正确，**表格被完整转成 markdown 表格**，下标用 HTML `<sub>` 保留：

```markdown
Assignment 2

1、**已知直线端点坐标为（12，10），（18，20），试用DDA算法确定直线上的所有像素坐标。**

2、**采用Bresenham算法画线，已知线段端点A（0,3）和E（3,10），试回答以下问题。**

**（1）写出m>1的Bresenham算法的决策参数递归公式。**

**（2）根据算法确定线段像素坐标，并填入表格中。**

| i | P<sub>i</sub> | (X<sub>i+1</sub>, Y<sub>i+1</sub>) |
| --- | --- | --- |
| 0 |  |  |
| 1 |  |  |
...
```

（节选）

## 8. 两种格式对比

| | `test.pdf`（3 页） | `test.docx`（全文） |
| --- | --- | --- |
| 输入大小 | 758482 B | 14790 B |
| 耗时 | 65.1 s | **0.6 s** |
| markdown 输出 | 18406 B | 749 B |
| 耗时主因 | 每页走图像/OCR 流程 | 纯文本直接抽取 |

DOCX 快约 100 倍。PDF 逐页处理，且扫描件需 OCR，`ocr_mode` 建议保持 `auto`。
两者 `content.markdown` 都是内联返回，无需二次请求。

## 9. 最小请求体

必填项只有两个：顶层 `files`，项内 `source`。而 `source` 每个分支都只需 2 个字段。

以 `file_id` 为例的最小可用请求体：

```json
{"files":[{"source":{"type":"file_id","file_id":"<FILE_ID>"}}]}
```

四种 source 各自的最小形态：

```json
{"files":[{"source":{"type":"file_id","file_id":"..."}}]}
{"files":[{"source":{"type":"url","url":"..."}}]}
{"files":[{"source":{"type":"inline","name":"a.pdf","data":"<base64>"}}]}
{"files":[{"source":{"type":"local","path":"/abs/path.pdf"}}]}
```

### 两个不能省的细节

**`type` 必须显式写。** OpenAPI schema 里标着 `"default": "file_id"`，但那是 Pydantic 生成 schema 时的产物，运行时判别器不做填充。省略后实测：

```
HTTP 400
{"error":{"code":"invalid_request",
  "message":"Unable to extract tag using discriminator 'type'\nfiles.0.source"}}
```

**`files` 必须是数组**，传单个对象不行：

```
HTTP 400
{"error":{"message":"1 validation error for CreateJobRequest\nfiles\n  Input should be a valid list"}}
```

三种缺法的报错：

| 缺什么 | 错误信息 |
| --- | --- |
| `source` | `files.0.source Field required` |
| `files` | `files Field required` |
| `type` | `Unable to extract tag using discriminator 'type'` |

### query 参数全有默认值

可以一个都不传，只有 `response_format=zip` 和 `page_range` 需要显式指定：

| 参数 | 默认 | 不给的后果 |
| --- | --- | --- |
| `response_format` | `json` | 想拿 zip 必须显式传 |
| `wait_timeout` | `300` | 同步等 300 秒，超时转 `202` |
| `tier` | 服务端配置（当前 `standard`） | |
| `ocr_mode` | `auto` | |
| `page_range` | 空（全文档） | |
| `output_formats` | `["markdown"]` | |

## 10. `202 Accepted` 与轮询

`wait_timeout` 是同步等待的秒数，超时后任务**继续在后台跑**，接口返回 `202`。客户端必须自己轮询，客户端库（如 reqwest）不会自动续等。

用 `wait_timeout=1` 配合 30 页 PDF 触发：

```
POST /v1/file_parse?wait_timeout=1
{"files":[{"source":{"type":"file_id","file_id":"file-9a1806385362e774660f373d"},"page_range":"1-30"}]}
```

`202`，耗时 1.0 s（即 `wait_timeout` 本身）：

```json
{
  "status": "running",
  "created_at": "2026-10-16T21:34:50Z",
  "started_at": "2026-10-16T21:34:50Z",
  "finished_at": null,
  "tier": "standard",
  "output_formats": ["markdown"],
  "access_level": "anonymous",
  "progress": {"completed": 0, "failed": 0, "total": 1},
  "files": [{
    "file_id": "file-9a1806385362e774660f373d",
    "name": "slow.pdf",
    "page_range": "1-30",
    "status": "queued",
    "parse": null,
    "output_files": null,
    "error": null
  }],
  "links": {
    "self": "/v1/tasks/job_70062f93f45ac536eeb0e878",
    "cancel": "/v1/tasks/job_70062f93f45ac536eeb0e878"
  },
  "task_id": "job_70062f93f45ac536eeb0e878",
  "status_url": "/v1/tasks/job_70062f93f45ac536eeb0e878",
  "result_url": "/v1/tasks/job_70062f93f45ac536eeb0e878/result"
}
```

`progress` 在 `202` 时为 `{0,0,total}`，`files[].status` 是 `queued`，`output_files` 为 `null`。

### 轮询收敛

`GET /v1/tasks/{task_id}` 与 `GET /v1/tasks/{task_id}/result` 返回相同内容：

```
[t=6s]  running   | progress: {"completed": 0, "failed": 0, "total": 1}
[t=12s] running   | progress: {"completed": 0, "failed": 0, "total": 1}
[t=18s] completed | progress: {"completed": 1, "failed": 0, "total": 1}
```

### 坑：轮询结果里 `content` 是空的

终态响应中 `content.markdown` 长度为 **0**，而 `output_files.markdown.bytes` 是 `1288276`。内容需再取一次：

```
GET /v1/files/{output_files.markdown.file_id}/content   → 直接返回 markdown 原文
```

源文件则取不到：

```
GET /v1/files/{源 file_id}/content
HTTP 403
{"error":{"code":"feature_requires_api_key","message":"Source files cannot be downloaded"}}
```

因此 `202` 的正确流程是三段：**轮询 task → 从 `output_files.markdown.file_id` 取内容**。
同步路径（`200`）能直接内联拿到 markdown，属同步完成时的特例。

### 终态判定

`status` 终态有三种，不要只判 `completed`，否则失败任务会死循环：

| 终态 | 含义 |
| --- | --- |
| `completed` | 成功，但仍需检查 `files[].error` |
| `failed` | 失败，看 `files[].error` |
| `cancelled` | 被取消（`DELETE /v1/tasks/{task_id}` 或 `links.cancel`） |

`finished_at` 为 `null` 表示仍在运行。

### 同步响应里的额外字段

`200` 响应还带有轮询态没有的字段，便于诊断：

```json
"progress": {"completed": 1, "failed": 0, "total": 1},
"files": [{
  "parse": {"model_used": null, "duration_ms": 13802, "parser_version": "4.0.12"}
}]
```

`files[].file_id` 也只在同步响应里出现。

## 11. 上传按 sha256 幂等去重

`POST /v1/uploads` 以 `sha256sum` 判重。内容相同则**直接返回已完成状态**：

```json
{
  "id": "upload_a6ebfd4062d1798b90d98beb",
  "status": "completed",
  "upload_url": null,
  "upload_method": null,
  "file": {"id": "file-97739a13160d0a95822fc420", "bytes": 14790}
}
```

此时 `upload_url` 为 `null`，再发 `PUT` 会失败：

```
HTTP 409
{"error":{"code":"upload_already_terminal","message":"Upload is completed"}}
```

**客户端必须先判 `status`**：已是 `completed` 就直接取 `file.id` 走 `file_parse`；
只有 `pending` 才继续 `PUT` + `complete`。

## 12. `mime_type` 不参与解析

传入完全错误的 MIME 不影响解析结果：

```
声明 mime_type = application/x-totally-wrong
→ 文件上传成功，file_parse 返回 completed
→ markdown 正常输出 749 字符，耗时 0.05 s
```

服务端不校验 `mime_type`，也不依赖它选择解析器（解析器按文件内容判定）。该字段只是
被原样存储并回显，供调用方事后查阅：

```json
"mime_type": "application/x-totally-wrong"
```

因此 MIME 取错不会导致失败，但会让记录失真。建议仍按服务端实际使用的映射表填写。

### MIME 映射表来源

权威来源是 MinerU 自身的 `MIME_TYPE_BY_EXTENSION`
（`venv/lib/python3.14/site-packages/mineru/filetypes.py`）：

| 后缀 | MIME |
| --- | --- |
| `pdf` | `application/pdf` |
| `ofd` | `application/ofd` |
| `epub` | `application/epub+zip` |
| `doc` | `application/msword` |
| `docx` | `application/vnd.openxmlformats-officedocument.wordprocessingml.document` |
| `ppt` | `application/vnd.ms-powerpoint` |
| `pptx` | `application/vnd.openxmlformats-officedocument.presentationml.presentation` |
| `xls` | `application/vnd.ms-excel` |
| `xlsx` | `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet` |
| `rtf` | `application/rtf` |
| `odt` | `application/vnd.oasis.opendocument.text` |
| `ods` | `application/vnd.oasis.opendocument.spreadsheet` |
| `odp` | `application/vnd.oasis.opendocument.presentation` |
| `html` / `htm` / `shtml` | `text/html` |
| `mhtml` / `mht` | `multipart/related` |
| `csv` | `text/csv` |
| `tsv` | `text/tab-separated-values` |
| `md` / `markdown` | `text/markdown` |
| `rst` | `text/x-rst` |
| `tex` | `text/x-tex` |
| `txt` | `text/plain` |

**不要用各语言标准库的对照表**，6 处不一致：

| 后缀 | 标准库 | MinerU |
| --- | --- | --- |
| `ofd` | 无 | `application/ofd` |
| `shtml` | 无 | `text/html` |
| `mhtml` | `message/rfc822` | `multipart/related` |
| `mht` | `message/rfc822` | `multipart/related` |
| `rst` | `text/prs.fallenstein.rst` | `text/x-rst` |
| `tex` | `application/x-tex` | `text/x-tex` |

`mhtml` 标准库给的是邮件 MIME，而 MHTML 是 MIME 实体打包，正确值是 `multipart/related`。
Rust 的 `mime_guess` 等 crate 同样源自 IANA，对这几个后缀给的值与服务端不一致。

## 13. 遗留对象

本次测试在服务端留下以下对象，未清理：

| 格式 | 类型 | id |
| --- | --- | --- |
| PDF | 上传（1 小时后自动过期） | `upload_36838fa63fbb3f71accff2f5` |
| PDF | 文件 | `file-0daba4e68fe97e9ab87218f9` |
| PDF | 任务 | `job_5f2275d16741485741624f6e` |
| PDF | 产物 | `file-5c1489ebba6abcdad762d6f2` |
| DOCX | 上传（1 小时后自动过期） | `upload_270f67f3c686aff2c03d7164` |
| DOCX | 文件 | `file-ebba75dec4d1b9470a51d095` |
| DOCX | 任务 | `job_cc4be89aa73efb66a97dd47d` |
| DOCX | 产物 | `file-6000e05a117052df098b3d11` |
| 错误 MIME 探测 | 文件 | `file-97739a13160d0a95822fc420` |
| 大 DOCX | 文件 | `file-c099519d2a7adf007954c713` |
| 大 DOCX | 产物 | `file-4038149dc90e5edd6a6c95a3` |
| PDF（202 触发） | 文件 | `file-9a1806385362e774660f373d` |
| PDF（202 触发） | 任务 | `job_70062f93f45ac536eeb0e878` |
| PDF（202 触发） | 产物 | `file-b314040e74baa564fb9ee24a` |

另有探测过程中的失败上传对象 `upload_b39feed1e21a8ff07ab7b08f`、`upload_eee636f71f2b30fd0dc4f42b`。

清理：

```
DELETE /v1/files/{file_id}
```
