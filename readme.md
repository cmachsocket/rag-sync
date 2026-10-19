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
```

多个目录共用一个 `output_dir`，靠别名区分：

| | 路径 |
|---|---|
| 产物 | `~/MinerU/work/2024/报告.pdf.md` |
| 上传名 | `work__报告.pdf.md` |

别名前缀不是冗余：LightRAG 以**文件名**为文档唯一键，看不到本地路径里的目录，
只分目录的话两个目录下的同名文件会撞成同一个键，后传的那个 409 自愈时会把先传成功的
那份从知识库里删掉。同一别名内的重名由用户保证不会出现。

启动时会校验配置，别名重复、目录重叠、别名含非法字符都直接报错。单个目录扫不通
（比如挂载盘没挂上）也会整轮中止 —— 漏扫在清理阶段等于「用户删了」，会把那个目录的产物与
知识库文档清空。

## 从单目录版本（`sync_dir`）升级

旧版是单数的 `sync_dir`，产物直接躺在 `output_dir` 下；新版改成 `sync_folders` 列表，
产物在 `output_dir/<别名>/`，上传名带 `别名__` 前缀。按下面顺序做，**别跳步**：

1. **备份** `rag-sync.db`。

2. **改配置**：`sync_dir: "~/Documents"` 换成

   ```yaml
   sync_folders:
     - name: "docs"
       path: "~/Documents"
   ```

3. **跑 migration**（给 `files` 加 `output_rel_path` 列）：

   ```sh
   diesel migration run
   ```

   没装 diesel CLI 就手动执行 `migrations/2026-10-17-000000-0001_add_output_rel_path/up.sql`，
   再往 `__diesel_schema_migrations` 插一行 `version = '202610170000000001'`。

4. **清旧布局产物**：`output_dir` 下那些不带别名层的 `X.ext.md` 挪走或删掉。
   新版只往 `output_dir/<别名>/` 写，留着旧的就是和新产物并存的重复内容。

5. **清知识库里的旧文档**：旧版上传名没有 `别名__` 前缀，新版认不出，不删就会和新上传的
   同名文档并存，检索命中重复块。用 `/documents/paginated` 拿 doc_id 列表，
   调 `/documents/delete_document` 删掉，然后把 `files.rag_id` 一起清空 ——
   **文档没了还留着 rag_id，清理阶段会因为「删一个不存在的文档」失败而永久卡住那一行**。

迁移前入库的行 `output_rel_path` 是 NULL。清理旧文件时取值顺序是：库里记的 →
按当前别名现算 → 两个候选（新路径 + 旧规则的无别名路径）各试一遍。所以**源文件仍在
原同步目录下**时不必回填；只有同时改过目录配置的行反推不出产物位置，会警告后跳过
产物清理（DB 行照删）。回填了则精确按当时的位置删。

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
