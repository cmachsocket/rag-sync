# tests/

集成测试全部抽离到这里，`src/` 下不再有 `#[cfg(test)] mod tests`。

之所以能这么放：crate 现在有 lib target（`src/lib.rs`），`main.rs` 只做 CLI 组装。
在此之前只有 bin target，`tests/` 里的代码根本 `use` 不到这些模块。

## 文件

| 文件 | 测什么 |
| --- | --- |
| `common/mod.rs` | 脚手架：临时 SQLite 库（跑真 migration）、临时目录、手写 mock HTTP 服务端 |
| `file_mime_test.rs` | 扩展名 → MIME 映射表 |
| `sync_config_test.rs` | 配置加载、字段校验、`run_id` 推导、`~` 展开 |
| `database_test.rs` | `DBfunc` 全部方法（真 SQLite） |
| `sync_folder_test.rs` | sha256、相对路径、目录扫描、`delete_old_files` |
| `mineru_test.rs` | MinerU 上传 / 解析 / 取产物，对 mock 服务端跑 |
| `lightrag_model_test.rs` | LightRAG 响应体反序列化与状态判定（纯函数） |
| `lightrag_requests_test.rs` | LightRAG 上传、409 自愈、删除、分页 |
| `sync_flow_test.rs` | 端到端：扫描 → 转换 → 写产物 → 上传 → 落库 |
| `regression_test.rs` | 回归防护：每个曾丢用户数据的缺陷一条 |
| `live_lightrag_test.rs` | 打真实服务的端到端测试，默认 `#[ignore]` |

## 错误分支的测法

下面几类分支容易被漏掉，各自的测法：

- **被吞掉的错误**（`unwrap_or(false)` / `eprintln` 后继续）：测「不该发生的清理确实没发生」。
  例：`清理_产物删不掉时保留数据库行` 用「产物路径上放一个目录」触发删除失败 —— 无需 root。
- **静默中止**：测它**确实中止**，并断言错误信息里带上了中止的理由。
  例：`扫描_子目录读失败时整轮中止`，以 root 运行时自动跳过而不是假装通过。
- **超时**：用 tokio 的自动推进时钟（`#[tokio::test(start_paused = true)]`），
  `READ_TIMEOUT` 是 60 s 真实时间，测试瞬时完成 —— 见 `http_客户端_对不回包的服务端会超时`。
- **错误信息丢上下文**：mock 返回非 2xx + 特定 body，断言正文出现在错误里。

## regression_test.rs

每条用例对应一个曾经真实存在的缺陷（多半会**删掉用户的原始文档**）。
修复后断言已翻转为正确行为，作用是挡回归 —— 其中一条红掉多半是真退化了，
别直接改断言去迁就。

## 跑

```sh
cargo test                      # 全部离线用例，不碰网络
cargo test --test known_issues_test   # 只看缺陷快照
```

## mock HTTP 服务端

`common/mod.rs` 里手写了最小 HTTP/1.1 服务端（含 chunked 请求体解码），
因为离线环境装不上 `wiremock` / `httpmock`。它会记录每个请求，
所以断言可以直接检查 method、path、header 和 body。
