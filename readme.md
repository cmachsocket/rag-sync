# rag-sync

把同步目录里的文档转成 Markdown，上传进 LightRAG 知识库。Rust 写的命令行工具。

流程：扫描 `sync_dir` → 交给 MinerU 解析成 Markdown（`txt`/`md` 直接透传）→ 写到 `output_dir` → 上传 LightRAG → 记录到本地 SQLite。源文件被删除时，自动清理产物、知识库文档和数据库记录。

## 安装

```sh
cargo build --release
```

## 配置

复制 `config.yml.example` 为 `config.yml`：

```yaml
sync_dir: "~/Documents"        # 要同步的目录
output_dir: "~/MinerU"         # Markdown 产物目录
mineru_url: "http://127.0.0.1:9777"
lightrag_url: "http://127.0.0.1:9778"
lightrag_username: "admin"
lightrag_password: "admin"
```

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
