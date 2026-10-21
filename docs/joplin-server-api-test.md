# Joplin Server 文件与同步接口测试记录

测试时间：2026-10-21
目标服务：`https://joplin.cmach.top`（Joplin Server v3，前面挂了 Cloudflare）
被测端点：文件接口 `GET /api/items/...`、同步接口 `GET /api/items/<路径>/delta`
测试账号：`cmach_socket@outlook.com`（密码与 `config.yml` 里 LightRAG 相同）
对照文档：官方 *Joplin Server delta sync*、*Structure of file URLs in Joplin Server*

**官方文档写的路径在这台服务器上大部分对不上。** 本文只写实测结果，与官方说法不一致的地方
单独标出；没有实测到的部分集中在最后一节。

## 1. 结论速查

| 想做的事 | 官方文档写的 | 这台服务器实测 |
| --- | --- | --- |
| 取变更事件 | `GET /api/files/delta?cursor=` | `GET /api/items/<路径>/delta?cursor=`（`/api/files/delta` → 404） |
| 按 id 取文件 | `BASE_URL/files/FILE_ID` | `GET /api/items/<路径>`；**裸 id 查不到**（404） |
| 按路径取文件 | `BASE_URL/files/root:/path/to/file.txt:` | `GET /api/items/root:/path/to/file.txt:` |
| 取文件内容 | 路径后加 `/content` | 同上 |
| 列目录 | 文档没提 | `GET /api/items/<路径>/children?limit=&cursor=` |
| 登录 | 文档没提 | `POST /api/sessions`（JSON），cookie 名 `sessionId` |

按 v3 的 OpenAPI 描述，服务端还有 `/api/locks`、`/api/shares`、`/api/batch` 等接口 ——
与「把笔记同步进 RAG」无关，本文不测。

### 同步客户端要遵守的几条

1. 登录拿 `sessionId` cookie，之后每个请求都带上（2.2）。
2. 枚举文件用 `GET /api/items/root:/:/children` 翻页 —— 返回的是**平铺的全部文件**，
   层级要靠 item 自己的 `parent_id` 还原（3.4）。
3. 取内容用 `GET /api/items/root:/<文件名>:/content`，返回的是**正文 + 末尾的元信息块**，
   要自己切开（第 5 节）。
4. **判断「这条笔记被删了」看 item 的 `deleted_time`，不是等 `type: 3`** —— 客户端删笔记
   在服务端只是软删除，`type: 3` 要等文件被真正清掉才出现（4.5）。
5. 增量用 `GET /api/items/root:/:/delta`：**用服务端给的 cursor 翻页、按 `has_more` 停**、
   要能认出 `resyncRequired`（4.2、4.3）。
6. 这台账号**没开 E2EE**，内容是明文，不用解密（7）。

## 2. 认证

### 2.1 登录

```bash
curl -X POST https://joplin.cmach.top/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{"email":"cmach_socket@outlook.com","password":"<密码>"}'
```

```json
{"id":"BAeoG5RnljxeWuqDJMqd7D","user_id":"8iI9YHJnorbl2qI39ylj08"}
```

| 情况 | 响应 |
| --- | --- |
| 邮箱密码正确 | `200` + `{"id": ..., "user_id": ...}` |
| 邮箱或密码不对 | `403` + `{"error":"Invalid email or password"}` |
| 邮箱不是注册邮箱 | 同上（不区分「邮箱不存在」和「密码错」） |

### 2.2 会话怎么带

**cookie 名是 `sessionId`**（驼峰），不是 v1 文档/老客户端用的 `session_id`：

```bash
curl -H 'Cookie: sessionId=BAeoG5RnljxeWuqDJMqd7D' \
  https://joplin.cmach.top/api/shares
```

实测过的带法：

| 带法 | 结果 |
| --- | --- |
| `Cookie: sessionId=<id>` | 正常 |
| `Cookie: session_id=<id>` | `403` + `{"error":"Forbidden"}` |
| `Cookie: JSESSIONID=<id>` | `403` |
| `Authorization: Bearer <id>` | `403` |
| `?session_id=<id>` | `403` |
| `X-Session-Id: <id>` | `403` |

`403 Forbidden` 与「资源不存在」的 `404` 长得不一样：没登录时所有需要认证的接口都回 403，
所以**看到 403 先怀疑会话没带上**，别当成权限问题。

## 3. 文件接口

### 3.1 路径写法

路径形如 `root:/<路径>:`，末尾的冒号不能少：

| 写法 | 结果 |
| --- | --- |
| `root:/:` | 根目录，正常 |
| `root/` | 等价于 `root:/:`，正常 |
| `root:/` | `400` + `{"error":"Invalid path format: root:"}` |
| `root:/<文件名>:` | 正常 |
| 裸文件 id（20 位或 32 位） | `404` + `{"error":"Not found: <id>"}` |

**裸 id 查不到文件。** 官方文档里「按 id 取文件」那条（`/files/FILE_ID`）在这台服务器上
用不了 —— `GET /api/items/<文件 id>` 一律 404。列目录和 delta 响应里都带 id，但那些 id
是服务端的内部 id，不能直接拿来拼 URL，只能用同一响应里的**文件名**。

### 3.2 元信息

```bash
curl -H 'Cookie: sessionId=<id>' \
  'https://joplin.cmach.top/api/items/root:/e95e148230c4408fa309b22f3bd6a9fb.md:'
```

```json
{
  "id": "GY6XleKTENCvOB49IRDksU",
  "name": "e95e148230c4408fa309b22f3bd6a9fb.md",
  "updated_time": 1792544941734,
  "created_time": 1792544941734
}
```

### 3.3 内容

```bash
curl -H 'Cookie: sessionId=<id>' \
  'https://joplin.cmach.top/api/items/root:/e95e148230c4408fa309b22f3bd6a9fb.md:/content'
```

返回文件原文（文本）。文件不存在 → `404` + `{"error":"Not found: root:/nonexistent.md:"}`。

### 3.4 列目录

```bash
curl -H 'Cookie: sessionId=<id>' \
  'https://joplin.cmach.top/api/items/root:/:/children?limit=100'
```

```json
{
  "items": [
    {"id": "GY6XleKTENCvOB49IRDksU", "name": "e95e148230c4408fa309b22f3bd6a9fb.md", "updated_time": 1792544941734},
    {"id": "eiaY3X9pGXR4WadB8Riop4", "name": "ff7f78334a2147949d951ae3e81b53b2.md", "updated_time": 1792544941707}
  ],
  "has_more": true,
  "cursor": "eyJsaW1pdCI6IjUiLCJvcmRlciI6W3siYnkiOiJ1cGRhdGVkX3RpbWUiLCJkaXIiOiJkZXNjIn1dLCJwYWdlIjoxfQ=="
}
```

- `cursor` 是 base64 的 JSON，解开是 `{"limit":"5","order":[{"by":"updated_time","dir":"desc"}],"page":1}`
  —— 翻页参数原样塞回去就行。
- 排序是 `updated_time` 倒序（新的在前）。

**这个列表是平的。** `root:/:/` 的 children 返回了账号下**全部 235 个文件**：笔记本、笔记、
资源、变更记录全在里面，`children` 不体现 Joplin 的层级。对某个具体文件取 children，
返回的就是它自己一个：

```bash
curl -H 'Cookie: sessionId=<id>' \
  'https://joplin.cmach.top/api/items/root:/37a87c9fc502411fa7cf1c0133096d87.md:/children?limit=5'
```

```json
{"items":[{"id":"NXTY9QnBXccNZxgulnPcOQ","name":"37a87c9fc502411fa7cf1c0133096d87.md","updated_time":1792543813156}],"has_more":false,"cursor":null}
```

笔记挂在哪个笔记本，只能从笔记自己的元信息里读 `parent_id`（见第 5 节）。

## 4. 同步接口（delta）

### 4.1 请求与响应

```bash
curl -H 'Cookie: sessionId=<id>' \
  'https://joplin.cmach.top/api/items/root:/:/delta?limit=5'
```

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

| 字段 | 说明 |
| --- | --- |
| `id` | 变更文件的**服务端 id**（20 位）。不能直接拿去拼 URL |
| `item_id` | Joplin 侧的 item id。实测既有 32 位十六进制（`cb7a660cea254ab1bb96311b2b6fc5ea`）也有老格式 20 位（`JoSSy9ABC79teutozvOktW`） |
| `item_name` | 文件名。**取内容靠它**：`/api/items/root:/<item_name>:/content` |
| `previous_share_id` | 共享相关的旧 share id，普通条目是空串 |
| `type` | 1 = 新建，2 = 更新，3 = 删除（见 4.5） |
| `updated_time` | 服务端记这条变更的时间（毫秒） |
| `jop_updated_time` | 客户端侧 item 的更新时间（毫秒） |

事件类型沿用官方文档的说法（create / update / delete），只是数值化成了 `type`。
官方文档里的**事件压缩**规则在这台服务器上实测存在（见 4.2）。

### 4.2 游标语义

| 场景 | 请求 | 结果 |
| --- | --- | --- |
| 首次同步 | 不带 `cursor` | 从头开始，返回最早的事件 |
| 接着翻页 | `cursor=<上一页返回的 cursor>` | 返回更晚的事件 |
| 翻到末尾 | `cursor=<末页返回的 cursor>` | `items: []`、`has_more: false`、cursor 不变 |

#### cursor 偶尔不等于本页最后一个事件的 id

第 4 页返回 58 条，最后一个事件的 id 是 `ZVukfitNLZfKBaDG6X8T3e`，而这一页给的 cursor 是
`liTjJDTgvmczsssm4OpJt9`。

**`liTjJDTgvmczsssm4OpJt9` 是一条真实存在的事件** —— 它是资源
`.resource/25609196cc244fd6a3e096c1d11c18f9` 的 update（`type: 2`），它前面紧挨着同一个
资源的 create（`type: 1`）。这两条按官方文档的**事件压缩**规则（`create - update` → `create`）
被服务端压成一条 create 返回，**被压掉的那条不进响应体，但 cursor 仍然指向它**。

验证过程：

| 实验 | 结果 |
| --- | --- |
| 用「最后一个**返回**事件的 id」当游标重走一遍 | 拿到 **349** 条，比服务端自己的翻页（348 条）多一条 |
| 多出来的那条 | 正是被压掉的资源 update `liTjJDTgvmczsssm4OpJt9` |
| 直接拿 `liTjJDTgvmczsssm4OpJt9` 当游标请求 | 返回的 32 条与正常翻页的第 5 页完全一样 |
| 拿第 4 页最后一个**返回**事件的 id 当游标请求 | 返回 33 条，第一条就是被压掉的那条 |
| 整条流里相邻的「同一 item 的 create + update」 | **只有这一对**；笔记的 create/update 之间都隔着别的行，所以没被压缩 |

底层 change 行一共 349 条（新建 232 + 更新 117），服务端压缩后返回 348 条（新建 232 + 更新 116）。
（数字是当时的快照，账号一直在同步，会漂。）

所以：

- **把 cursor 原样传回去翻页是对的**，不会漏东西 —— 被压掉的那条本来就是压缩的结果
  （官方文档里 `create - update` → `create` 就是这个意思，客户端按「这条 item 有新版本」处理即可）。
- **不要把 cursor 当成「我已经处理过的最后一条事件」**：它可能指向一条从未发给你的事件，
  拿它做水位判断或去重会算错。
- **必须用服务端给的 cursor 翻页**，别自己拿「最后一个返回事件的 id」当游标：压缩落在页内
  还是页边会改变结果 —— 实测同一条流，服务端 cursor 翻出 391 条，自己拿最后一个事件的 id
  翻出 386 条（少 5 条，正好是页边界上被压缩掉的那几条）。
- **翻页只能按 `has_more` 停**：末页返回的 cursor 再要一次，拿到的是空页、`has_more` 仍是
  `false`、cursor 不变。

#### 事件流本身

同一个 `item_id` 会出现多次（这个账号里 115 个 item 只出现一次、117 个出现两次），
事件按时间从早到晚排，**同一个 `item_id` 的最后一条事件才是它的最新状态**。
（这组数字同样是快照。）

### 4.3 游标失效

游标无效（乱码、`0`、指向已被清掉的变更）时：

```json
{"error":"Delta cursor is invalid and the complete data should be resynced","code":"resyncRequired"}
```

状态码是 **400**，错误码在 `code` 字段里 —— 与官方文档描述的行为一致，客户端要认这个
`code`，丢掉游标从头同步。实测 `cursor=0` 和 `cursor=nonexistentcursor` 都回这个。

### 4.4 作用域

官方文档说「变更事件挂在某个 parent id 上，只能列出某个目录（非递归）的变更」。
**实测不成立**：把笔记本的路径塞进 delta 的路径部分

```bash
curl -H 'Cookie: sessionId=<id>' \
  'https://joplin.cmach.top/api/items/root:/37a87c9fc502411fa7cf1c0133096d87.md:/delta?limit=3'
```

返回的还是根目录那一条流（第一条事件与 `root:/:/` 完全一样）。也就是说 v3 的 delta
**只有全局一档**，按目录做增量同步暂时做不到。

### 4.5 删除事件（`type: 3`）

用桌面端（剪贴板 API `POST /notes` → `DELETE /notes/:id`）在账号里造了一次真实的
「建笔记 → 删笔记」，再把那个文件从服务端的存储里删掉，观察 delta：

**第一步：客户端「删除笔记」= 软删除，不是删除事件。**

客户端删掉笔记后同步上去，服务端这边发生的是：

- 文件**还在**（`children` 里还能列到，内容也还能取到）；
- item 的 `deleted_time` 被置上（`deleted_time: 1792551787908`），`updated_time` 跟着变；
- delta 里出现的是一条 **update（`type: 2`）**，而且因为紧跟在 create 后面，被压缩规则
  压成了 create（`create - update` → `create`），响应里只看到那条 create，
  cursor 指向被压掉的 update。

**所以：客户端判断「这条笔记被删了」要看 item 的 `deleted_time` 是不是非 0，
不能等 `type: 3`。** Joplin 客户端删笔记后默认保留 30 天才彻底清掉，
这段时间服务端一直留着文件和正文。

**第二步：文件被真正删掉后，才出现 `type: 3`。**

```bash
curl -X DELETE -H 'Cookie: sessionId=<id>' \
  'https://joplin.cmach.top/api/items/root:/121b02cc118a485d924d4ec2b521726c.md:'
```

之后 delta 里那条 item 的事件变成：

```json
{
  "id": "zJb3oIBf94pvcd2G1fZrnQ",
  "item_id": "y0XYv4S0pr6P5DUX0SPbSy",
  "item_name": "121b02cc118a485d924d4ec2b521726c.md",
  "previous_share_id": "",
  "type": 3,
  "updated_time": 1792551869455
}
```

| 字段 | 说明 |
| --- | --- |
| `type` | 3 = 删除 |
| `item_name` | 被删掉的文件名 —— **客户端靠它知道删的是哪个** |
| `item_id` | 服务端给这个文件的 id（20 位），不是 Joplin 的 item id |
| `jop_updated_time` | **没有这个字段**（其余事件都有） |
| `updated_time` | 服务端记这次删除的时间（毫秒） |

另外两个实测细节：

- **之前那条 create 事件被这条 delete 顶掉了**：同一条流里这个 item 只剩 `type: 3` 一条，
  原来的 create（`type: 1`）不见了。官方文档里说的「服务端发现某条 create/update 事件
  对应的文件已经不在了，就回一条 delete 事件」，实测就是这个效果。
- **删完就彻底没了**：`children` 里不再有这个文件，`GET .../content` 回
  `404 {"error":"Not found: root:/121b02cc118a485d924d4ec2b521726c.md:"}`。

官方文档提到的「delete 事件可能被发两次」（先因为文件不在了发一次，真正删掉后再发一次）
这次没复现 —— 第一次看到这条 item 的事件时文件还在，所以只等到了一条 `type: 3`。

## 5. 存储模型（实测）

每个 item 存成一个 `.md` 文件，**文件名是 `<id>.md`**（`id` 是 32 位十六进制），内容是
**正文 + 末尾的元信息块**。元信息块是若干 `key: value` 行，最后一行是 `type_`。

账号下 203 个 `.md` 文件**全部**是这个结构：每个都有元信息块，且块里的 `id` 与文件名
逐一对上，没有例外。

item 自己的文件长这样（正文在前，元信息块在后）：

```text
优化建图方法

目前只会线段树优化建图

## 线段树优化建图
...

id: 98f2e8b73e3043f1bced03d6ff497429
parent_id: 97851e06ae0c4b7096a1f4a6228f2168
created_time: 2024-01-31T0...
...
type_: 1
```

**变更记录**是另一种文件（`type_: 13`），描述「item `cb7a660…` 相对上一版改了什么」：

```text
id: e95e148230c4408fa309b22f3bd6a9fb
parent_id:
item_type: 1
item_id: cb7a660cea254ab1bb96311b2b6fc5ea
item_updated_time: 1792544317197
title_diff: "[]"
body_diff: "[]"
metadata_diff: {"new":{"id":"cb7a660cea254ab1bb96311b2b6fc5ea","parent_id":"771a5c7c11514f37b28dc0524424ebf6",...},"deleted":[]}
encryption_cipher_text:
encryption_applied: 0
updated_time: 2026-10-21T01:06:13.188Z
created_time: 2026-10-21T01:06:13.188Z
is_locked: 0
type_: 13
```

两种文件里都有 `id`（这条记录自己的 id，和文件名一致），只有**变更记录**有 `item_id`
（指向被改的那个 item）。

要点：

| 项 | 实测 |
| --- | --- |
| item 文件的元信息键 | `id` / `parent_id` / `type_` / `created_time` / `updated_time` / `user_created_time` / `encryption_applied` / `encryption_cipher_text` / `is_locked` / `markup_language` / `share_id` / `master_key_id` / `extracted_resource_ids` / `latitude` / `longitude` … |
| 变更记录的元信息键 | `id` / `item_id` / `item_type` / `item_updated_time` / `title_diff` / `body_diff` / `metadata_diff` / `type_: 13` |
| `type_` 的取值 | 1 = 笔记（143 个）、2 = 笔记本（19 个）、4 = 资源（29 个）、5 = 标签（1 个）、6 = 笔记的历史版本（Revision）；13 = 变更记录（10 个） |
| 笔记的标题 | 元信息块里**没有标题字段**。正文第一行就是标题（`优化建图方法`） |
| `parent_id` | 父 item 的 id（笔记挂在哪个笔记本上靠它） |
| `title_diff` / `body_diff` | diff-match-patch 风格的差异：`{"diffs":[[1,"插入的文本"]],"start1":0,"start2":0,"length1":0,"length2":7}`；`1` = 插入、`-1` = 删除、`0` = 保留 |
| `metadata_diff` | `{"new": {...}, "deleted": [...]}`，键是 item 的元信息字段 |
| 资源 | 二进制在 `.resource/<resource_id>`，元信息在 `<resource_id>.md`。取二进制：`/api/items/root:/.resource/<resource_id>:/content` |
| 其他文件 | `info.json`、`.sync/version.txt`、`.sync/readme.txt` |

账号下 235 个文件的构成：203 个 `.md`（193 个 item 文件 + 10 个变更记录）+ 29 个资源二进制 +
`info.json` + `.sync/version.txt` + `.sync/readme.txt`。

正文和元信息块的分界：**从文件末尾往上找连续的 `key: value` 行**，且这段里必须同时有
`id`、`parent_id`、`type_` 三个键，否则整份文件都当正文。正文里出现 `备注: xxx` 这种行
时，光看「结尾是 key: value」会把正文尾巴切掉。

另外，delta 事件里的 `item_id` 与文件名的 id **不一定相同**：这个账号里有一批老格式
item（id 20 位，如 `JoSSy9ABC79teutozvOktW`），服务端换成了 32 位十六进制的 id 和文件名，
但 delta 事件里报的仍是老 id。

## 6. 能跑通的最小流程

```bash
# 1. 登录，拿到 cookie
COOKIE="sessionId=$(curl -sS -X POST https://joplin.cmach.top/api/sessions \
  -H 'Content-Type: application/json' \
  -d '{"email":"cmach_socket@outlook.com","password":"<密码>"}' \
  | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')"

# 2. 列出全部文件
curl -sS -H "Cookie: $COOKIE" \
  'https://joplin.cmach.top/api/items/root:/:/children?limit=100'

# 3. 取一个文件的正文（正文 + 元信息块）
curl -sS -H "Cookie: $COOKIE" \
  'https://joplin.cmach.top/api/items/root:/e95e148230c4408fa309b22f3bd6a9fb.md:/content'

# 4. 拿变更流（不带 cursor = 从头）
curl -sS -H "Cookie: $COOKIE" \
  'https://joplin.cmach.top/api/items/root:/:/delta?limit=100'
```

## 7. 未验证 / 待确认

- **压缩规则的具体边界**：只实测到 `create - update` → `create` 这一条。官方文档里的
  `update - update` → `update`、`create - delete` → 不发，没有对应样本可验。
- **E2EE**：这个账号**没开 E2EE** —— 桌面端同步信息里写着 `"e2ee":{"value":false}`，
  所有条目的 `encryption_applied` 都是 `0`、`encryption_cipher_text` 全空，
  `GET /api/users/<用户 id>/public_key` 返回 200 但正文为空。同步进 RAG 不需要解密；
  开了 E2EE 的账号怎么取 master key、怎么解密，未验证。
- **裸 id 访问**：确认 20 位服务端 id、32 位 item id、用户 id 都查不到文件，但不确定
  有没有别的按 id 取文件的写法。
- **子目录 delta 作用域**：实测不生效（见 4.4），不确定是 v3 的设计还是这台服务器的部署问题。
- **`type_: 6` 是什么**：笔记的历史版本（Revision）—— 桌面端同步日志里能看到
  `createRemote: ... Revision: (Local 2151a322...)`。同步进 RAG 时要跳过它。
- **写接口**：除了 4.5 用到的 `DELETE /api/items/<路径>:`，其余写操作没测。
  顺带发现 `PUT /api/items/<路径>:/content`（multipart，字段名 `content`）能建文件，
  但建出来的是**没有 change 行的空文件**，不会出现在 delta 里 —— 造不出真实的 item。
- **本文里的文件数 / 事件数是当时的快照**：账号是活的，桌面端一直在同步，数字会漂。
