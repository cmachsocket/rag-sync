# rag-sync

把同步目录里的文档转成 Markdown，上传进 LightRAG 知识库；另有一条独立的数据源：
Joplin Server 的笔记，走同步流增量取，**不在本地落盘**。Rust 写的命令行工具。

目录那条流程：扫描 `sync_folders` 里的每个目录 → 交给 MinerU 解析成 Markdown（`txt`/`md` 直接透传）→ 写到 `output_dir/<别名>/` → 上传 LightRAG → 记录到本地 SQLite（含产物相对路径 `output_rel_path`）。源文件被删除时，自动清理产物、知识库文档和数据库记录。

Joplin 那条流程：读服务端给的变更流（delta）→ 按文件名取笔记正文 → 正文只在内存里，直接传进 LightRAG → 记下笔记状态与游标。笔记被删了，知识库里的文档一起撤掉。

## 安装

```sh
cargo build --release
```

## 配置

复制 `config.yml.example` 为 `config.yml`：

```yaml
sync_folders:                # 要同步的目录，每个配一个别名
  - name: "work"             # 别名：产物子目录名 + 上传文件名前缀
    path: "~/Documents/work"
  - name: "personal"
    path: "~/Documents/personal"
output_dir: "~/MinerU"       # 所有别名共用一个产物目录
mineru_url: "http://127.0.0.1:9777"
lightrag_url: "http://127.0.0.1:9778"
lightrag_username: "admin"
lightrag_password: "admin"
mineru_concurrency: 1        # 同时解析几个文件（MinerU）
lightrag_concurrency: 1      # 同时上传几个文件（LightRAG）
```

Joplin 同步是可选的，配置里加一段 `joplin` 才有：

```yaml
joplin:
  url: "https://joplin.cmach.top"
  email: "you@example.com"
  password: "..."
  alias: "joplin"            # 上传文件名前缀：joplin__<item_name>
  limit: 100                 # delta 每页取几条变更事件
```

多个目录共用一个 `output_dir`，靠别名区分：

| | 路径 |
|---|---|
| 产物 | `~/MinerU/work/2024/报告.pdf.md` |
| 上传名 | `work__报告.pdf.md` |

别名前缀不是冗余：LightRAG 以**文件名**为文档唯一键，看不到本地路径里的目录，
只分目录的话两个目录下的同名文件会撞成同一个键，后传的那个 409 自愈时会把先传成功的
那份从知识库里删掉。

启动时会校验配置，别名重复、目录重叠、别名含非法字符都直接报错。单个目录扫不通
（比如挂载盘没挂上）也会整轮中止 —— 漏扫在清理阶段等于「用户删了」，会把那个目录的产物与
知识库文档清空。

同一别名内的文件夹里面的文件重名由用户保证不会出现。

## 同步流程

一轮同步是**两步流水线**，两步同时跑：

```
源文件 ──[第一步：转换]──> 产物 .md ──[第二步：上传]──> 知识库
   (透传 / MinerU 解析)     落盘到 output_dir     (LightRAG)
```

上一个文件还在知识库里排队抽取时，下一个文件已经在解析了。拆开的另一个好处是
**失败分开算**：产物一落盘就把「解析过」记进库，上传失败（同一轮还是下一轮）都只补传，
不再把解析重跑一遍（大文件解析一次几十分钟）；解析失败也不会去动知识库。

每一步内部还能再并发几份，由 `mineru_concurrency` / `lightrag_concurrency` 控制，
默认都是 1。调高之前要知道代价：

- `mineru_concurrency`：逐页 OCR 是重活（实测 6.4 秒/页），并发会压垮解析器；
  而且「某个文件重试耗尽后本轮不再开始后面的文件」这个保证依赖它。
- `lightrag_concurrency`：LightRAG 的写入走单流水线，删文档（409 自愈要用）
  在流水线被占着时会回 `busy` —— 而占着它的很可能就是自己这几份并发上传。

### 同名冲突（409）

LightRAG 以文件名为唯一键，同名就 409。库里额外记了「上次传进知识库的那份内容的
sha256」（`files.uploaded_sha256`，注意它不是源文件的 sha256），靠它分清两种情况：

- **内容没变**（源文件改了，但解析出来的正文一模一样）—— 冲突的那份就是我们要的那份，
  直接沿用它的 doc_id，**不删不重传**。删了重传会让文档在知识库里短暂缺席，
  还得重新抽一遍；传失败还会把旧文档弄丢，而库里还指着它。
- **内容变了** —— 旧文档必须让位，删了重传（老规矩）。

`uploaded_sha256` 是 NULL 的行（加列之前入库的）没有依据，一律按老规矩删了重传。

## Joplin 同步

配了 `joplin` 段才会跑。它和目录同步互不影响，各自是一条独立的数据源。

**走的是服务端的变更流（delta）。** `GET /api/items/root:/:/delta` 按服务端给的
cursor 翻页，按 `has_more` 停。cursor 必须原样传回去 —— 它可能指向一条被压缩掉、
从未下发给客户端的事件，自己拿「最后一条返回事件的 id」当游标会漏东西。

**笔记不落盘。** 取到的正文去掉末尾的元信息块，直接进知识库，本地一个文件都不留。
上传名是 `<别名>__<item_name>`（`item_name` 是服务端上的文件名）。

**只有笔记进库。** 变更记录、历史版本、笔记本、资源都跳过。

**删掉了怎么办。** 两种情况都算删：

- delta 里有 `type: 3`（文件被彻底删掉）；
- 笔记的 `deleted_time` 非 0（客户端删了但文件还留着，默认保留 30 天）。

两种都会把知识库里的文档撤掉，并删掉库里的记录。

**状态存在库里。** `joplin_state` 存游标，`joplin_notes` 存每篇笔记的内容指纹与
doc_id。内容没变的笔记不重传。游标失效（服务端回 `resyncRequired`）时从头全量
同步一遍，靠内容指纹认出哪些不用重传。

## 运行

```sh
cargo run
```

每轮同步使用递增的 `run_id`，某轮失败则不推进计数器，下轮从同一断点续跑：
已同步的按 sha256 与入库内容指纹跳过，解析过但还没进知识库的复用产物、只补上传。

## 测试

```sh
cargo test
```

集成测试在 `tests/`，详见 `docs/tests.md`。
