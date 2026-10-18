//! 集成测试的公共脚手架。
//!
//! 三个东西：
//!   1. `test_db` —— 临时 SQLite 库 + 真实 migration，不用仓库里的 rag-sync.db。
//!   2. `MockServer` —— 手写的最小 HTTP/1.1 服务端，用来替 MinerU / LightRAG。
//!      离线环境拿不到 wiremock / httpmock，所以自己实现；只依赖已有的 tokio。
//!   3. `SyncConfig` 之类的构造辅助。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use diesel_async::SimpleAsyncConnection;
use rag_sync::database::DBfunc;
use rag_sync::sync_config::SyncConfig;

// ---------------------------------------------------------------- 临时目录 / 库

/// 用真实的 migration 建表。改 migration 时测试跟着变，
/// 不会出现「测试里写死一份 DDL、线上 schema 已经变了」的假绿。
pub const MIGRATION_UP: &str = include_str!("../../migrations/2026-10-16-181726-0000_init/up.sql");

pub struct TestDb {
    pub conn: rag_sync::database::Conn,
    pub path: PathBuf,
    // 持有 TempDir，drop 时连库文件一起删掉
    _dir: tempfile::TempDir,
}

impl TestDb {
    /// 绕过 diesel 直接塞一条 files 记录，用来摆前置状态。
    /// 参数都是 SQL 字面量，测试自己控制，不构成注入面。
    pub async fn insert_file_row(&mut self, id: i32, path: &str, sha256: &str, run_id: i32, rag_id: Option<&str>) {
        let rag = rag_id.map(|r| format!("'{r}'")).unwrap_or_else(|| "NULL".into());
        self.conn
            .batch_execute(&format!(
                "INSERT INTO files (id, path, sha256, rag_id, run_id) VALUES ({id}, '{path}', '{sha256}', {rag}, {run_id});"
            ))
            .await
            .unwrap();
    }

    /// 该行的 run_id。路径不存在会 panic —— 用例本身就是要看这行。
    pub async fn run_id_of(&mut self, path: &str) -> i32 {
        DBfunc::get_file_run_id(&mut self.conn, path)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("files 里没有 {path} 这行"))
    }

    /// 库里所有行，用 `found_file_less_than_run_id(i32::MAX)` 一次捞干净。
    pub async fn rows(&mut self) -> Vec<rag_sync::db_model::File> {
        DBfunc::found_file_less_than_run_id(&mut self.conn, i32::MAX)
            .await
            .unwrap()
    }

    pub async fn row_ids(&mut self) -> Vec<i32> {
        self.rows().await.iter().map(|f| f.id).collect()
    }
}

/// 每个用例一个独立的临时库文件，避免并发用例互相污染。
pub async fn test_db() -> TestDb {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.db");
    let conn = rag_sync::database::establish_db_connection_at(path.to_str().unwrap())
        .await
        .unwrap();
    let mut conn = conn;
    conn.batch_execute(MIGRATION_UP).await.unwrap();
    TestDb {
        conn,
        path,
        _dir: dir,
    }
}

// ---------------------------------------------------------------- 配置

/// 指向临时目录的配置，URL 由 MockServer 填。
pub struct Env {
    pub root: tempfile::TempDir,
    pub sync_dir: String,
    pub output_dir: String,
    pub mineru_url: String,
    pub lightrag_url: String,
}

impl Env {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let sync_dir = root.path().join("sync");
        let output_dir = root.path().join("out");
        std::fs::create_dir_all(&sync_dir).unwrap();
        Self {
            sync_dir: sync_dir.to_string_lossy().into_owned(),
            output_dir: output_dir.to_string_lossy().into_owned(),
            mineru_url: String::new(),
            lightrag_url: String::new(),
            root,
        }
    }

    pub fn config(&self, run_id: i32) -> SyncConfig {
        SyncConfig {
            sync_dir: self.sync_dir.clone(),
            output_dir: self.output_dir.clone(),
            mineru_url: self.mineru_url.clone(),
            run_id,
            lightrag_password: "pass".into(),
            lightrag_url: self.lightrag_url.clone(),
            lightrag_username: "user".into(),
        }
    }

    /// 在 sync 目录下写文件，自动建父目录。返回绝对路径字符串。
    pub fn write_source(&self, rel: &str, content: &str) -> String {
        let p = Path::new(&self.sync_dir).join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        p.to_string_lossy().into_owned()
    }

    pub fn output_path(&self, rel: &str) -> PathBuf {
        Path::new(&self.output_dir).join(rel)
    }

    pub fn output_dir(&self) -> &Path {
        Path::new(&self.output_dir)
    }

    pub fn output_text(&self, rel: &str) -> String {
        std::fs::read_to_string(self.output_path(rel)).unwrap()
    }
}

// ---------------------------------------------------------------- mock HTTP 服务端

#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub method: String,
    /// 原始 request-target，含 query
    pub target: String,
    /// 键一律小写
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl CapturedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == lower)
            .map(|(_, v)| v.as_str())
    }

    pub fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// 请求体是 JSON 时的便捷断言；解析失败会 panic，把响应内容一起带出来。
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body_str())
            .unwrap_or_else(|e| panic!("请求体不是 JSON: {e}\nbody = {:?}", self.body_str()))
    }
}

#[derive(Debug, Clone)]
pub struct CannedResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl CannedResponse {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.into(),
        }
    }

    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Route {
    pub method: String,
    pub target: String,
    pub responses: Vec<CannedResponse>,
    /// 队列取空后重复最后一个响应。轮询类接口需要；只想匹配 N 次的场景用 `once`。
    pub repeat_last: bool,
}

impl Route {
    pub fn new(method: &str, target: &str) -> Self {
        Self {
            method: method.to_ascii_uppercase(),
            target: target.to_string(),
            responses: Vec::new(),
            repeat_last: true,
        }
    }

    pub fn get(target: &str) -> Self {
        Self::new("GET", target)
    }

    pub fn post(target: &str) -> Self {
        Self::new("POST", target)
    }

    pub fn put(target: &str) -> Self {
        Self::new("PUT", target)
    }

    pub fn delete(target: &str) -> Self {
        Self::new("DELETE", target)
    }

    pub fn and(mut self, r: CannedResponse) -> Self {
        self.responses.push(r);
        self
    }

    /// 队列取空后一律 404，用来断言「不该再发请求」。
    pub fn once(mut self) -> Self {
        self.repeat_last = false;
        self
    }
}

struct RouteState {
    method: String,
    target: String,
    queue: std::collections::VecDeque<CannedResponse>,
    repeat_last: bool,
}

impl RouteState {
    fn next(&mut self) -> Option<CannedResponse> {
        if self.queue.len() > 1 {
            return self.queue.pop_front();
        }
        if self.queue.len() == 1 {
            let only = self.queue.pop_front().unwrap();
            if self.repeat_last {
                self.queue.push_back(only.clone());
            }
            return Some(only);
        }
        None
    }
}

pub struct MockServer {
    pub base_url: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<CapturedRequest>>>,
    routes: std::sync::Arc<std::sync::Mutex<Vec<RouteState>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockServer {
    pub async fn start(routes: Vec<Route>) -> Self {
        use std::sync::Arc;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let route_states = Arc::new(std::sync::Mutex::new(
            routes
                .into_iter()
                .map(|r| RouteState {
                    method: r.method,
                    target: r.target,
                    queue: r.responses.into(),
                    repeat_last: r.repeat_last,
                })
                .collect::<Vec<_>>(),
        ));

        let handle = {
            let requests = requests.clone();
            let route_states = route_states.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let requests = requests.clone();
                    let route_states = route_states.clone();
                    tokio::spawn(async move {
                        let Some(req) = read_request(&mut stream).await else {
                            return;
                        };
                        requests.lock().unwrap().push(req.clone());
                        let resp = pick(&route_states, &req);
                        let _ = write_response(&mut stream, &resp).await;
                    });
                }
            })
        };

        Self {
            base_url: format!("http://{addr}"),
            requests,
            routes: route_states,
            handle,
        }
    }

    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub fn requests_to(&self, method: &str, target: &str) -> Vec<CapturedRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.method.eq_ignore_ascii_case(method) && r.target == target)
            .collect()
    }

    pub fn count_to(&self, method: &str, target: &str) -> usize {
        self.requests_to(method, target).len()
    }

    /// 按顺序打印打过的所有请求，断言失败时用来对现场。
    pub fn dump(&self) -> String {
        self.requests()
            .iter()
            .map(|r| format!("{} {} -> {:?}", r.method, r.target, r.body_str()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
        let _ = &self.routes;
    }
}

fn pick(routes: &std::sync::Arc<std::sync::Mutex<Vec<RouteState>>>, req: &CapturedRequest) -> CannedResponse {
    let mut routes = routes.lock().unwrap();
    for r in routes.iter_mut() {
        if r.method.eq_ignore_ascii_case(&req.method) && r.target == req.target {
            if let Some(resp) = r.next() {
                return resp;
            }
        }
    }
    CannedResponse::json(
        404,
        format!("no route for {} {}", req.method, req.target),
    )
}

async fn write_response(stream: &mut tokio::net::TcpStream, resp: &CannedResponse) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp.status,
        reason(resp.status),
        resp.content_type,
        resp.body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(resp.body.as_bytes()).await?;
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

// ------------------------------------------------- 最小 HTTP/1.1 请求解析（含 chunked）

struct ByteReader<'a> {
    stream: &'a mut tokio::net::TcpStream,
    buf: Vec<u8>,
    pos: usize,
}

impl<'a> ByteReader<'a> {
    fn new(stream: &'a mut tokio::net::TcpStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            pos: 0,
        }
    }

    /// 往 buf 末尾追加一次真正的读。EOF / 出错返回 false。
    /// 不会丢弃已读但没消费的字节 —— `read_n` 依赖这点。
    async fn read_more(&mut self) -> bool {
        use tokio::io::AsyncReadExt;
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        let mut tmp = [0u8; 16 * 1024];
        match self.stream.read(&mut tmp).await {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.buf.extend_from_slice(&tmp[..n]);
                true
            }
        }
    }

    /// 保证 buf 里至少还有一个字节，返回 false 表示对端关闭。
    async fn fill(&mut self) -> bool {
        if self.pos < self.buf.len() {
            return true;
        }
        self.read_more().await
    }

    /// 读一行，不含结尾 CRLF。
    async fn read_line(&mut self) -> Option<String> {
        let mut line = Vec::new();
        loop {
            if let Some(rel) = find(&self.buf[self.pos..], b"\r\n") {
                line.extend_from_slice(&self.buf[self.pos..self.pos + rel]);
                self.pos += rel + 2;
                return Some(String::from_utf8_lossy(&line).into_owned());
            }
            line.extend_from_slice(&self.buf[self.pos..]);
            self.pos = self.buf.len();
            if !self.read_more().await {
                return None;
            }
        }
    }

    /// 读 n 字节；EOF 时返回已读到的部分。
    async fn read_n(&mut self, n: usize) -> Vec<u8> {
        while self.buf.len() - self.pos < n {
            if !self.read_more().await {
                break;
            }
        }
        let end = (self.pos + n).min(self.buf.len());
        let out = self.buf[self.pos..end].to_vec();
        self.pos = end;
        out
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<CapturedRequest> {
    let mut r = ByteReader::new(stream);

    let request_line = r.read_line().await?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let mut headers = Vec::new();
    loop {
        let line = r.read_line().await?;
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }

    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };

    let body = if header("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        let mut out = Vec::new();
        loop {
            let Some(line) = r.read_line().await else { break };
            let hex = line.trim().split(';').next().unwrap_or("0");
            let Ok(size) = usize::from_str_radix(hex, 16) else {
                break;
            };
            if size == 0 {
                break;
            }
            out.extend_from_slice(&r.read_n(size).await);
            let _ = r.read_line().await;
        }
        out
    } else if let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        r.read_n(len).await
    } else {
        Vec::new()
    };

    Some(CapturedRequest {
        method,
        target,
        headers,
        body,
    })
}
