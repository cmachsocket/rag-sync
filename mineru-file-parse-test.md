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

## 9. 遗留对象

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

清理：

```
DELETE /v1/files/{file_id}
```
