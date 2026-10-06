//! Shared test fixtures: a dependency-free stub HTTP server and temp projects.
#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// One canned response.
#[derive(Debug, Clone)]
pub struct StubResponse {
    /// Status code.
    pub status: u16,
    /// Reason phrase.
    pub reason: &'static str,
    /// Extra headers, in order.
    pub headers: Vec<(String, String)>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl StubResponse {
    /// A response with no headers or body.
    pub fn new(status: u16, reason: &'static str) -> StubResponse {
        StubResponse {
            status,
            reason,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// A `application/json` response.
    pub fn json(status: u16, reason: &'static str, body: &str) -> StubResponse {
        StubResponse::new(status, reason)
            .header("Content-Type", "application/json")
            .body(body)
    }

    /// Adds a header.
    pub fn header(mut self, name: &str, value: &str) -> StubResponse {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Sets the body.
    pub fn body(mut self, body: &str) -> StubResponse {
        self.body = body.as_bytes().to_vec();
        self
    }
}

/// A request the stub server received.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// Request method.
    pub method: String,
    /// Request target (path plus query).
    pub target: String,
    /// Request headers as sent, in order.
    pub headers: Vec<(String, String)>,
    /// Request body bytes.
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body as text.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body parsed as JSON.
    pub fn body_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("recorded body is JSON")
    }

    /// Headers with `name: value` lower-cased names, for easier assertions.
    pub fn header_names(&self) -> Vec<String> {
        self.headers
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect()
    }
}

/// A single-threaded HTTP/1.1 stub server bound to an ephemeral port.
pub struct StubServer {
    /// `http://127.0.0.1:<port>` for this server.
    pub base_url: String,
    /// Requests received so far, in order.
    pub received: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl StubServer {
    /// Starts a server answering with the responses built from its base URL.
    pub fn start(build: impl FnOnce(&str) -> Vec<StubResponse>) -> StubServer {
        Self::spawn(build, false)
    }

    /// Starts a server that accepts requests and never answers.
    pub fn start_silent() -> StubServer {
        Self::spawn(|_| Vec::new(), true)
    }

    fn spawn(build: impl FnOnce(&str) -> Vec<StubResponse>, silent: bool) -> StubServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub server binds");
        let address = listener.local_addr().expect("local addr");
        let base_url = format!("http://{address}");
        let responses = build(&base_url);
        let received = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let sink = Arc::clone(&received);
        let stop = Arc::clone(&shutdown);
        listener
            .set_nonblocking(true)
            .expect("listener can be non-blocking");
        let join = thread::spawn(move || {
            let mut queue = responses.into_iter();
            let mut current = queue.next();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).expect("stream is blocking");
                        let recorded = read_request(stream.try_clone().expect("clone"));
                        if let Some(recorded) = recorded {
                            sink.lock().expect("not poisoned").push(recorded);
                        }
                        if silent {
                            while !stop.load(Ordering::SeqCst) {
                                thread::sleep(Duration::from_millis(2));
                            }
                            return;
                        }
                        if let Some(response) = current.take() {
                            write_response(stream, &response);
                            current = queue.next();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => return,
                }
            }
        });
        StubServer {
            base_url,
            received,
            shutdown,
            join: Some(join),
        }
    }

    /// Requests received so far.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.received.lock().expect("not poisoned").clone()
    }

    /// Requests received so far, one per call, without cloning all of them.
    pub fn request_count(&self) -> usize {
        self.received.lock().expect("not poisoned").len()
    }

    /// Stops the server and returns every request it received.
    pub fn finish(mut self) -> Vec<RecordedRequest> {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }

        self.requests()
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn read_request(mut stream: TcpStream) -> Option<RecordedRequest> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        head.push_str(&line);
    }
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    let _ = stream.flush();
    Some(RecordedRequest {
        method,
        target,
        headers,
        body,
    })
}

fn write_response(mut stream: TcpStream, response: &StubResponse) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.body.len()
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&response.body);
    let _ = stream.flush();
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A named directory under the temp dir, removed on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a fresh directory.
    pub fn new(label: &str) -> TempDir {
        let unique = format!(
            "curlyfries-it-{}-{}-{label}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp dir is creatable");
        TempDir { path }
    }

    /// The directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Joins a relative path.
    pub fn child(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }

    /// Writes a file, creating parent directories.
    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.child(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent directory is creatable");
        }
        fs::write(&path, contents).expect("file is writable");
        path
    }

    /// Creates a directory.
    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.child(relative);
        fs::create_dir_all(&path).expect("directory is creatable");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Path of the compiled `curlyfries` binary.
pub fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_curlyfries"))
}

/// Runs the binary in `dir` with piped (non-terminal) standard streams.
pub fn run_binary(dir: &Path, args: &[&str]) -> Output {
    Command::new(binary())
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("curlyfries binary runs")
}

/// Exit code of a finished process.
pub fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("process exited normally")
}

/// stdout of a finished process as text.
pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// stderr of a finished process as text.
pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Discovers a project and its requests.
pub fn load_project(
    dir: &TempDir,
) -> (
    curlyfries::project::Project,
    Vec<curlyfries::project::Entry>,
) {
    let project =
        curlyfries::project::discover(dir.path(), None).expect("test project is discoverable");
    let entries =
        curlyfries::project::collect_requests(&project).expect("test requests are collected");
    (project, entries)
}

/// Resolves the project's environment the way the CLI would without `--env`.
pub fn environment(project: &curlyfries::project::Project) -> (Option<String>, curlyfries::Map) {
    match curlyfries::project::resolve_environment(project, None).expect("environment resolves") {
        Some((name, vars)) => (Some(name), vars),
        None => (None, curlyfries::Map::new()),
    }
}

/// Builds a runner for in-process tests.
pub fn runner<'a>(
    project: &'a curlyfries::project::Project,
    entries: &'a [curlyfries::project::Entry],
    session: &'a mut curlyfries::session::Session,
    env_name: Option<String>,
    env: curlyfries::Map,
) -> curlyfries::runner::Runner<'a> {
    let timeout = Some(Duration::from_secs(5));
    curlyfries::runner::Runner {
        project,
        entries,
        env_name,
        env,
        overrides: curlyfries::Map::new(),
        session,
        client: curlyfries::execute::client(timeout),
        timeout,
        warnings: Vec::new(),
    }
}

/// A project pointing at `server` with `dev` as the default environment.
pub fn project_fixture(label: &str, server: &StubServer, files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new(label);
    dir.write("curlyfries.json", r#"{ "defaultEnvironment": "dev" }"#);
    dir.write(
        "environments/dev.json",
        &format!(r#"{{ "baseUrl": "{}", "limit": 2 }}"#, server.base_url),
    );
    for (relative, contents) in files {
        dir.write(relative, contents);
    }
    dir
}
