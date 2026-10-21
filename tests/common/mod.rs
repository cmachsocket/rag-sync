//! 集成测试的公共脚手架：`test_db`（临时 SQLite 库 + 真实 migration，不碰仓库里的
//! rag-sync.db）、`MockServer`（手写最小 HTTP/1.1 服务端，离线环境拿不到 wiremock）、
//! `Env`（临时 sync/out 目录与配置构造）。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use diesel_async::SimpleAsyncConnection;
use rag_sync::database::DBfunc;
use rag_sync::sync_config::{SyncConfig, SyncFolder};

// 临时目录 / 库

/// 用真实的 migration 建表，避免测试里写死 DDL 而线上 schema 已经变了。
/// 按版本号顺序执行，和 diesel 自己跑的顺序一致。
pub const MIGRATIONS_UP: &[&str] = &[
    include_str!("../../migrations/2026-10-16-181726-0000_init/up.sql"),
    include_str!("../../migrations/2026-10-17-000000-0001_add_output_rel_path/up.sql"),
    include_str!("../../migrations/2026-10-20-000000-0002_add_uploaded_sha256/up.sql"),
    include_str!("../../migrations/2026-10-21-000000-0003_add_joplin_state/up.sql"),
];

pub struct TestDb {
    pub conn: rag_sync::database::Conn,
    pub path: PathBuf,
    // 持有 TempDir，drop 时连库文件一起删掉
    _dir: tempfile::TempDir,
}

impl TestDb {
    /// 绕过 diesel 直塞一条 files 记录，用来摆前置状态；参数都是 SQL 字面量。
    /// `output_rel_path` 传 None 就是「加列之前写入的行」。
    pub async fn insert_file_row(
        &mut self,
        id: i32,
        path: &str,
        sha256: &str,
        run_id: i32,
        rag_id: Option<&str>,
    ) {
        self.insert_file_row_full(id, path, sha256, run_id, rag_id, None)
            .await;
    }

    pub async fn insert_file_row_full(
        &mut self,
        id: i32,
        path: &str,
        sha256: &str,
        run_id: i32,
        rag_id: Option<&str>,
        output_rel_path: Option<&str>,
    ) {
        let rag = rag_id
            .map(|r| format!("'{r}'"))
            .unwrap_or_else(|| "NULL".into());
        let rel = output_rel_path
            .map(|r| format!("'{r}'"))
            .unwrap_or_else(|| "NULL".into());
        self.conn
            .batch_execute(&format!(
                "INSERT INTO files (id, path, sha256, rag_id, run_id, output_rel_path) \
                 VALUES ({id}, '{path}', '{sha256}', {rag}, {run_id}, {rel});"
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

    /// 补上「知识库里这份内容的指纹」。模拟「这一份已经传过了」。
    pub async fn set_uploaded_sha256(&mut self, path: &str, sha256: &str) {
        self.conn
            .batch_execute(&format!(
                "UPDATE files SET uploaded_sha256 = '{sha256}' WHERE path = '{path}';"
            ))
            .await
            .unwrap();
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
    for up in MIGRATIONS_UP {
        conn.batch_execute(up).await.unwrap();
    }
    TestDb {
        conn,
        path,
        _dir: dir,
    }
}

// 配置

/// 指向临时目录的配置，URL 由 MockServer 填。默认带一个别名 `sync` 的同步目录，
/// 产物落在 `out/sync/`。
pub struct Env {
    pub root: tempfile::TempDir,
    pub sync_dir: String,
    pub output_dir: String,
    pub folders: Vec<SyncFolder>,
    pub mineru_url: String,
    pub lightrag_url: String,
    /// 同时解析几个（MinerU）、同时上传几个（LightRAG）。默认都是 1。
    pub mineru_concurrency: usize,
    pub lightrag_concurrency: usize,
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
            folders: vec![SyncFolder {
                name: "sync".into(),
                path: sync_dir.to_string_lossy().into_owned(),
            }],
            mineru_url: String::new(),
            lightrag_url: String::new(),
            mineru_concurrency: 1,
            lightrag_concurrency: 1,
            root,
        }
    }

    /// 再加一个别名目录，返回它的绝对路径。目录之间不重叠，够用了。
    pub fn add_folder(&mut self, name: &str) -> String {
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let path = path.to_string_lossy().into_owned();
        self.folders.push(SyncFolder {
            name: name.into(),
            path: path.clone(),
        });
        path
    }

    /// 第一个（默认 `sync`）别名的目录名 —— `output_path` 用它解析相对路径。
    pub fn primary_alias(&self) -> &str {
        &self.folders[0].name
    }

    pub fn config(&self, run_id: i32) -> SyncConfig {
        SyncConfig {
            sync_folders: self.folders.clone(),
            output_dir: self.output_dir.clone(),
            mineru_url: self.mineru_url.clone(),
            run_id,
            lightrag_password: "pass".into(),
            lightrag_url: self.lightrag_url.clone(),
            lightrag_username: "user".into(),
            joplin: None,
            mineru_concurrency: self.mineru_concurrency,
            lightrag_concurrency: self.lightrag_concurrency,
        }
    }

    /// 允许同时解析/上传几份。默认 1 —— 流水线本身的重叠不靠这个。
    pub fn with_concurrency(&mut self, mineru: usize, lightrag: usize) -> &mut Self {
        self.mineru_concurrency = mineru;
        self.lightrag_concurrency = lightrag;
        self
    }

    /// 在主同步目录下写文件，自动建父目录。返回绝对路径字符串。
    pub fn write_source(&self, rel: &str, content: &str) -> String {
        self.write_source_in(self.primary_alias(), rel, content)
    }

    /// 在指定别名目录下写文件。返回绝对路径字符串。
    pub fn write_source_in(&self, alias: &str, rel: &str, content: &str) -> String {
        let folder = self
            .folders
            .iter()
            .find(|f| f.name == alias)
            .unwrap_or_else(|| panic!("没有别名为 {alias} 的同步目录"));
        let p = Path::new(&folder.path).join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        p.to_string_lossy().into_owned()
    }

    /// 产物路径。`rel` 不含别名目录名，按主别名解析。
    pub fn output_path(&self, rel: &str) -> PathBuf {
        self.output_path_in(self.primary_alias(), rel)
    }

    /// 指定别名下的产物路径。
    pub fn output_path_in(&self, alias: &str, rel: &str) -> PathBuf {
        Path::new(&self.output_dir).join(alias).join(rel)
    }

    pub fn output_dir(&self) -> &Path {
        Path::new(&self.output_dir)
    }

    pub fn output_text(&self, rel: &str) -> String {
        std::fs::read_to_string(self.output_path(rel)).unwrap()
    }

    /// 直接写一个产物文件（模拟上一轮留下的），父目录一并建出来。
    pub fn write_artifact(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.output_path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        p
    }

    /// 指定别名下的产物内容。
    pub fn output_text_in(&self, alias: &str, rel: &str) -> String {
        std::fs::read_to_string(self.output_path_in(alias, rel)).unwrap()
    }
}

// mock HTTP 服务端

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
    /// 放行闸门：有它的话，响应要等闸门开了才发出去。
    pub gate: Option<Gate>,
}

impl CannedResponse {
    pub fn json(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.into(),
            gate: None,
        }
    }

    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.into(),
            gate: None,
        }
    }

    /// 响应卡在服务端，直到闸门被放行。用来把「请求已到达」和「客户端拿到响应」
    /// 分开：测流水线是否真的重叠时，让某一步卡住不放，再看另一步有没有在动。
    pub fn gated(mut self, gate: Gate) -> Self {
        self.gate = Some(gate);
        self
    }
}

/// 放行闸门：一个信号量，开了就放行一个等待中的响应。
#[derive(Clone)]
pub struct Gate {
    inner: std::sync::Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gate")
    }
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
        }
    }

    /// 放行所有等待中的响应，以及之后的所有响应（开了就一直开着）。
    pub fn open(&self) {
        self.inner.add_permits(1024);
    }

    async fn wait(&self) {
        let permit = self
            .inner
            .acquire()
            .await
            .expect("信号量不会关闭：闸门只加许可，不关闸门");
        // 吃掉这个许可：放行一次就该少一个，不能攒着变成「以后都放行」
        permit.forget();
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
    /// mock 只监听回环地址，请求**必须**直连。reqwest 默认读 HTTP_PROXY /
    /// HTTPS_PROXY，打向 127.0.0.1 的请求可能被塞给环境里的代理，表现为
    /// 时好时坏的 502，且和被测代码毫无关系。
    pub fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("构造测试用 HTTP 客户端失败")
    }

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

    /// 等到某个接口被请求到指定次数为止（超时也没等到就返回实际次数）。
    /// 流水线类断言要靠它：只能等，不能立刻查 —— 立刻查会查到还没发生的将来。
    pub async fn wait_for_count(
        &self,
        method: &str,
        target: &str,
        want: usize,
        timeout: std::time::Duration,
    ) -> usize {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let n = self.count_to(method, target);
            if n >= want {
                return n;
            }
            if tokio::time::Instant::now() >= deadline {
                return n;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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

fn pick(
    routes: &std::sync::Arc<std::sync::Mutex<Vec<RouteState>>>,
    req: &CapturedRequest,
) -> CannedResponse {
    let mut routes = routes.lock().unwrap();
    for r in routes.iter_mut() {
        if r.method.eq_ignore_ascii_case(&req.method)
            && r.target == req.target
            && let Some(resp) = r.next()
        {
            return resp;
        }
    }
    CannedResponse::json(404, format!("no route for {} {}", req.method, req.target))
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    resp: &CannedResponse,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    // 闸门没开就先把响应扣住：客户端这时候正等着，读不到东西
    if let Some(gate) = &resp.gate {
        gate.wait().await;
    }
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

// 最小 HTTP/1.1 请求解析（含 chunked）

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

    /// 往 buf 末尾追加一次真正的读。EOF / 出错返回 false；不丢弃未消费的字节。
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
    haystack.windows(needle.len()).position(|w| w == needle)
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
            let Some(line) = r.read_line().await else {
                break;
            };
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
