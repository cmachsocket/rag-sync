# rag-sync

把同步目录里的文档转成 Markdown，上传进 LightRAG 知识库。Rust 写的命令行工具。

流程：扫描 `sync_folders` 里的每个目录 → 交给 MinerU 解析成 Markdown（`txt`/`md` 直接透传）→ 写到 `output_dir/<别名>/` → 上传 LightRAG → 记录到本地 SQLite（含产物相对路径 `output_rel_path`）。源文件被删除时，自动清理产物、知识库文档和数据库记录。

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
**失败分开算**：上传失败只重传，不再把解析重跑一遍（大文件解析一次几十分钟）；
解析失败也不会去动知识库。

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

## 运行

```sh
cargo run
```

每轮同步使用递增的 `run_id`，某轮失败则不推进计数器，下轮从同一断点续跑（已同步的按 sha256 跳过）。

## 测试

```sh
cargo test
```

集成测试在 `tests/`，详见 `tests/README.md`。
