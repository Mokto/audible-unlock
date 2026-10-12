mod login_browser;

use gpui::{
    App, Application, Bounds, Context, Div, ElementId, ExternalPaths, FocusHandle, KeyBinding,
    PathPromptOptions, SharedString, Stateful, StyledImage, Window, WindowBounds, WindowOptions,
    actions, div, img, prelude::*, px, rgb, size,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

actions!(
    unlock,
    [AddFiles, Connect, Logout, PasteBytes, Convert, Cancel, Quit]
);

struct Worker {
    input: mpsc::Sender<Value>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn project_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let resources = exe.parent().unwrap().join("../Resources/backend");
        if resources.join("pyproject.toml").is_file() {
            return resources;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn process_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| format!("{}:", dir.display())))
        .unwrap_or_default();
    format!(
        "{bundled}/opt/homebrew/bin:/usr/local/bin:{home}/.local/bin:{home}/.cargo/bin:{}",
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into())
    )
}

fn start_worker(args: Vec<String>, events: mpsc::Sender<Value>) -> Worker {
    let (input, incoming) = mpsc::channel::<Value>();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelled = cancel.clone();
    thread::spawn(move || {
        let root = project_root();
        let path = process_path();
        let uv = path
            .split(':')
            .map(|dir| Path::new(dir).join("uv"))
            .find(|candidate| candidate.is_file());
        let Some(uv) = uv else {
            let _ = events.send(json!({"kind":"error", "message":"uv is missing. Install it with: brew install uv"}));
            let _ = events.send(json!({"kind":"finished"}));
            return;
        };
        let child = Command::new(uv)
            .args(["run", "--locked", "--project"])
            .arg(&root)
            .args(["python", "-u"])
            .arg(root.join("backend/worker.py"))
            .args(args)
            .env("PATH", path)
            .env_remove("VIRTUAL_ENV")
            .env(
                "UV_PROJECT_ENVIRONMENT",
                PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join("Library/Caches/audible-unlock/python"),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        let Ok(mut child) = child else {
            let _ = events
                .send(json!({"kind":"error", "message":"Unable to start the Python backend."}));
            let _ = events.send(json!({"kind":"finished"}));
            return;
        };
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (lines, output) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str::<Value>(&line)
                    && lines.send(value).is_err()
                {
                    break;
                }
            }
        });
        let mut cancel_at = None;
        let mut terminal = false;
        loop {
            for message in incoming.try_iter() {
                let _ = writeln!(stdin, "{message}");
                let _ = stdin.flush();
            }
            if cancelled.load(Ordering::Relaxed) && cancel_at.is_none() {
                let _ = writeln!(stdin, "{}", json!({"kind":"cancel"}));
                let _ = stdin.flush();
                cancel_at = Some(Instant::now());
            }
            if cancel_at.is_some_and(|at| at.elapsed() > Duration::from_secs(3)) {
                // Kill the entire process group so a stuck FFmpeg cannot outlive the app.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                let _ = events.send(json!({"kind":"cancelled"}));
                break;
            }
            match output.recv_timeout(Duration::from_millis(50)) {
                Ok(value) => {
                    terminal |= matches!(
                        value["kind"].as_str(),
                        Some(
                            "error"
                                | "done"
                                | "connected"
                                | "logged_out"
                                | "status"
                                | "cancelled"
                                | "library"
                        )
                    );
                    let _ = events.send(value);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.wait();
                    if !terminal {
                        let _ = events.send(json!({"kind":"error", "message":"The backend stopped unexpectedly. Run uv sync in the project folder and try again."}));
                    }
                    break;
                }
            }
        }
        let _ = events.send(json!({"kind":"finished"}));
    });
    Worker { input, cancel }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Checking,
    LoginOpening,
    LoginWaiting,
    LoginFinishing,
    SavingBytes,
    LoggingOut,
    LoadingLibrary,
    Converting,
}

impl Operation {
    fn is_login(self) -> bool {
        matches!(
            self,
            Self::LoginOpening | Self::LoginWaiting | Self::LoginFinishing
        )
    }
}

fn conversion_blocker(
    operation: Option<Operation>,
    connected: bool,
    dependencies: bool,
) -> Option<&'static str> {
    match operation {
        Some(Operation::Checking) => Some("Checking local tools…"),
        Some(operation) if operation.is_login() => {
            Some("Finish or cancel sign-in before converting.")
        }
        Some(Operation::SavingBytes) => Some("Saving activation bytes…"),
        Some(Operation::LoggingOut) => Some("Logging out…"),
        Some(Operation::LoadingLibrary) => Some("Loading your Audible library…"),
        Some(Operation::Converting) => {
            Some("Conversion is running. The next book starts automatically.")
        }
        _ if !dependencies => {
            Some("FFmpeg is missing. Rebuild the app bundle or run: brew install ffmpeg")
        }
        _ if !connected => Some("Connect to Audible or paste activation bytes before converting."),
        _ => None,
    }
}

#[derive(PartialEq)]
enum State {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

struct Book {
    path: PathBuf,
    asin: Option<String>,
    title: String,
    state: State,
    detail: String,
    progress: f32,
    output: Option<PathBuf>,
}

#[derive(Clone)]
struct LibraryBook {
    asin: String,
    title: String,
    detail: String,
    cover: Option<PathBuf>,
}

fn cover(path: Option<PathBuf>) -> gpui::AnyElement {
    let size = px(56.);
    match path {
        Some(path) => img(path)
            .size(size)
            .flex_shrink_0()
            .rounded_md()
            .object_fit(gpui::ObjectFit::Cover)
            .with_fallback(move || {
                div()
                    .size(size)
                    .rounded_md()
                    .bg(rgb(0x253148))
                    .into_any_element()
            })
            .into_any_element(),
        None => div()
            .size(size)
            .flex_shrink_0()
            .rounded_md()
            .bg(rgb(0x253148))
            .into_any_element(),
    }
}

fn library_books(value: &Value) -> Vec<LibraryBook> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|book| {
            let minutes = book["minutes"].as_u64().unwrap_or(0);
            let authors = book["authors"].as_str().unwrap_or_default();
            let length = format!("{} h {:02} min", minutes / 60, minutes % 60);
            Some(LibraryBook {
                asin: book["asin"].as_str()?.into(),
                title: book["title"].as_str()?.into(),
                detail: if authors.is_empty() {
                    length
                } else {
                    format!("{authors} · {length}")
                },
                cover: book["cover"].as_str().map(PathBuf::from),
            })
        })
        .collect()
}

struct Unlock {
    focus: FocusHandle,
    books: Vec<Book>,
    library: Option<Vec<LibraryBook>>,
    library_ready: bool,
    show_library: bool,
    connected: bool,
    dependencies: bool,
    locale: String,
    message: String,
    login_url: Option<String>,
    browser: Option<login_browser::LoginBrowser>,
    login_session: u64,
    worker: Option<Worker>,
    operation: Option<Operation>,
    cancelling: bool,
    sender: mpsc::Sender<Value>,
    events: mpsc::Receiver<Value>,
    active: Option<usize>,
    queue_running: bool,
    operation_failed: bool,
}

impl Unlock {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (sender, events) = mpsc::channel();
        let focus = cx.focus_handle();
        focus.focus(window);
        cx.on_app_quit(|this, cx| {
            this.queue_running = false;
            this.browser = None;
            if let Some(worker) = &this.worker {
                worker.cancel.store(true, Ordering::Relaxed);
            }
            cx.background_executor().timer(Duration::from_millis(90))
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let worker = start_worker(vec!["status".into()], sender.clone());
        Self {
            focus,
            books: vec![],
            library: None,
            library_ready: false,
            show_library: false,
            connected: false,
            dependencies: false,
            locale: "uk".into(),
            message: "Checking local tools…".into(),
            login_url: None,
            browser: None,
            login_session: 0,
            worker: Some(worker),
            operation: Some(Operation::Checking),
            cancelling: false,
            sender,
            events,
            active: None,
            queue_running: false,
            operation_failed: false,
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let events: Vec<_> = self.events.try_iter().collect();
        if events.is_empty() {
            return;
        }
        for event in events {
            if event["kind"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("webview_"))
                && event["session"].as_u64() != Some(self.login_session)
            {
                continue;
            }
            let text = |key: &str| event[key].as_str().unwrap_or_default().to_owned();
            let embedded_waiting =
                self.operation == Some(Operation::LoginWaiting) && !self.cancelling;
            match text("kind").as_str() {
                "status" => {
                    self.connected = event["connected"].as_bool().unwrap_or(false);
                    self.library_ready = event["library"] == true;
                    self.dependencies = event["ffmpeg"] == true && event["ffprobe"] == true;
                    self.message = if !self.dependencies {
                        "FFmpeg is missing. Rebuild the app bundle or run: brew install ffmpeg"
                            .into()
                    } else if self.connected {
                        "Ready. Add AAX files to get started.".into()
                    } else {
                        "Connect to Audible or paste your activation bytes.".into()
                    };
                }
                "login_url" => {
                    if self.cancelling {
                        continue;
                    }
                    self.login_url = Some(text("url"));
                    self.operation = Some(Operation::LoginWaiting);
                    self.open_login_window();
                }
                "webview_opened" if embedded_waiting => {
                    self.message = "Sign in in the Amazon window. The login result will be captured automatically.".into();
                }
                "webview_redirect" if embedded_waiting => self.submit_redirect(text("value")),
                "webview_closed" if embedded_waiting => self.stop(),
                "webview_error" if embedded_waiting => {
                    self.browser = None;
                    self.message = "The sign-in window could not start. Retry with Show sign-in window, or cancel and log in again.".into();
                }
                "webview_ready" if embedded_waiting => {
                    self.message = "Complete Amazon sign-in in the window. The result will be captured automatically.".into();
                }
                "webview_load_failed" | "webview_navigation_blocked" if embedded_waiting => {
                    self.message = "Amazon sign-in could not load. Use Retry sign-in in that window, or cancel and try again.".into();
                }
                "connected" => {
                    self.browser = None;
                    self.connected = true;
                    self.library_ready = event["library"] == true;
                    self.login_url = None;
                    self.message = "Logged in. Add AAX files to begin converting.".into();
                }
                "logged_out" => {
                    self.connected = false;
                    self.books.clear();
                    self.library = None;
                    self.library_ready = false;
                    self.show_library = false;
                    self.browser = None;
                    self.login_url = None;
                    self.queue_running = false;
                    self.message = "Logged out. Activation bytes and the Keychain sign-in were removed; your audiobook files are unchanged.".into();
                }
                "library" => {
                    let books = library_books(&event["books"]);
                    self.message = format!(
                        "{} downloadable {} in your library.",
                        books.len(),
                        if books.len() == 1 { "book" } else { "books" }
                    );
                    self.library = Some(books);
                }
                "cover" => {
                    if let Some(book) = self
                        .library
                        .iter_mut()
                        .flatten()
                        .find(|book| book.asin == text("asin"))
                    {
                        book.cover = Some(PathBuf::from(text("path")));
                    }
                }
                "download" => {
                    if let Some(index) = self.active {
                        let received = event["received"].as_f64().unwrap_or(0.0);
                        let total = event["total"].as_f64().unwrap_or(0.0);
                        let book = &mut self.books[index];
                        book.progress = if total > 0.0 {
                            (received / total) as f32
                        } else {
                            0.0
                        };
                        book.detail = if total > 0.0 {
                            format!(
                                "Downloading · {:.0} / {:.0} MB",
                                received / 1e6,
                                total / 1e6
                            )
                        } else {
                            "Requesting download from Audible…".into()
                        };
                    }
                }
                "downloaded" => {
                    if let Some(index) = self.active {
                        self.books[index].path = PathBuf::from(text("path"));
                        self.books[index].progress = 0.0;
                        self.books[index].detail = "Downloaded · reading chapters…".into();
                    }
                }
                "book" => {
                    if let Some(index) = self.active {
                        self.books[index].title = text("title");
                        self.books[index].detail = format!("0 / {} chapters", event["total"]);
                    }
                }
                "progress" => {
                    if let Some(index) = self.active {
                        let completed = event["completed"].as_u64().unwrap_or(0);
                        let total = event["total"].as_u64().unwrap_or(1).max(1);
                        self.books[index].progress = completed as f32 / total as f32;
                        self.books[index].detail =
                            format!("{completed} / {total} chapters · {}", text("chapter"));
                    }
                }
                "done" => {
                    if let Some(index) = self.active {
                        self.books[index].state = State::Done;
                        self.books[index].progress = 1.0;
                        self.books[index].output = Some(PathBuf::from(text("output")));
                        self.books[index].detail = "Complete · lossless M4A".into();
                        self.message = format!(
                            "Finished {}. The original AAX is unchanged.",
                            self.books[index].title
                        );
                    }
                }
                "warning" => {
                    self.message = text("message");
                    if self.operation == Some(Operation::LoginFinishing) {
                        self.operation = Some(Operation::LoginWaiting);
                    }
                }
                "error" => {
                    self.browser = None;
                    self.operation_failed = true;
                    self.message = text("message");
                    if let Some(index) = self.active {
                        self.books[index].state = State::Failed;
                        self.books[index].detail = self.message.clone();
                    }
                }
                "cancelled" => {
                    self.browser = None;
                    self.operation_failed = true;
                    self.queue_running = false;
                    self.login_url = None;
                    self.message = if self.operation.is_some_and(Operation::is_login) {
                        if self.connected {
                            "Sign-in cancelled. Your saved activation bytes still work; you can convert the queue.".into()
                        } else {
                            "Sign-in cancelled. Your queued books are unchanged.".into()
                        }
                    } else if self.operation == Some(Operation::Converting) {
                        "Stopped. Completed books and original files are unchanged; partial downloads were discarded."
                            .into()
                    } else if self.operation == Some(Operation::LoadingLibrary) {
                        "Library loading cancelled.".into()
                    } else {
                        "Setup cancelled. Your queued books and saved activation bytes are unchanged.".into()
                    };
                    if let Some(index) = self.active
                        && self.books[index].state != State::Done
                    {
                        self.books[index].state = State::Cancelled;
                        self.books[index].detail = "Cancelled · retry when ready".into();
                    }
                }
                "finished" => {
                    self.browser = None;
                    self.worker = None;
                    self.operation = None;
                    self.cancelling = false;
                    self.login_url = None;
                    self.active = None;
                    if self.operation_failed {
                        self.queue_running = false;
                    }
                    if self.queue_running {
                        self.next();
                    }
                }
                _ => {}
            }
        }
        cx.notify();
    }

    fn add_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if !self.connected || self.operation == Some(Operation::LoggingOut) {
            return;
        }
        for path in paths {
            if !path.is_file()
                || !path.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("aax") || ext.eq_ignore_ascii_case("aaxc")
                })
            {
                self.message = "Only .aax and .aaxc files are supported.".into();
                continue;
            }
            let path = path.canonicalize().unwrap_or_else(|_| path.clone());
            if self.books.iter().any(|book| book.path == path) {
                continue;
            }
            self.books.push(Book {
                title: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                path,
                asin: None,
                state: State::Queued,
                detail: "Queued".into(),
                progress: 0.0,
                output: None,
            });
        }
        cx.notify();
    }

    fn add_files(&mut self, _: &AddFiles, _: &mut Window, cx: &mut Context<Self>) {
        if !self.connected || self.operation == Some(Operation::LoggingOut) {
            return;
        }
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add AAX or AAXC audiobooks".into()),
        });
        cx.spawn(async move |this, cx| match picker.await {
            Ok(Ok(Some(paths))) => {
                let _ = this.update(cx, |this, cx| this.add_paths(&paths, cx));
            }
            Ok(Ok(None)) => {}
            _ => {
                let _ = this.update(cx, |this, cx| {
                    this.message =
                        "Unable to open the file picker. Drag files into the window instead."
                            .into();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn connect(&mut self, _: &Connect, _: &mut Window, cx: &mut Context<Self>) {
        if self.connected || self.worker.is_some() {
            return;
        }
        self.operation_failed = false;
        self.cancelling = false;
        self.operation = Some(Operation::LoginOpening);
        self.login_session = self.login_session.wrapping_add(1);
        self.message = "Opening a secure Amazon sign-in window…".into();
        self.worker = Some(start_worker(
            vec!["login".into(), self.locale.clone()],
            self.sender.clone(),
        ));
        cx.notify();
    }

    fn logout(&mut self, _: &Logout, _: &mut Window, cx: &mut Context<Self>) {
        if !self.connected || self.worker.is_some() {
            return;
        }
        self.login_session = self.login_session.wrapping_add(1);
        self.operation_failed = false;
        self.cancelling = false;
        self.operation = Some(Operation::LoggingOut);
        self.message = "Logging out and forgetting saved activation bytes…".into();
        self.worker = Some(start_worker(vec!["logout".into()], self.sender.clone()));
        cx.notify();
    }

    fn open_login_window(&mut self) {
        let Some(url) = &self.login_url else { return };
        if let Some(browser) = &mut self.browser {
            browser.show();
            return;
        }
        match login_browser::LoginBrowser::open(url, self.login_session, self.sender.clone()) {
            Ok(browser) => {
                self.browser = Some(browser);
                self.message =
                    "Opening the Amazon sign-in window. Its result will be captured automatically."
                        .into();
            }
            Err(_) => {
                self.message =
                    "Could not open the sign-in window. Retry or cancel and log in again.".into()
            }
        }
    }

    fn submit_redirect(&mut self, value: String) {
        if self.operation != Some(Operation::LoginWaiting) || self.cancelling {
            return;
        }
        if let Some(worker) = &self.worker {
            let _ = worker.input.send(json!({"kind":"redirect", "value":value}));
            self.operation = Some(Operation::LoginFinishing);
            self.browser = None;
            self.message = "Login result received. Fetching activation bytes…".into();
        }
    }

    fn paste_bytes(&mut self, _: &PasteBytes, _: &mut Window, cx: &mut Context<Self>) {
        if self.connected || self.worker.is_some() {
            return;
        }
        if let Some(value) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let value = value.trim();
            if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                self.message = "Copy exactly eight hexadecimal activation characters first.".into();
            } else {
                self.operation_failed = false;
                let worker = start_worker(vec!["save".into()], self.sender.clone());
                let _ = worker.input.send(json!({"kind":"bytes", "value":value}));
                self.worker = Some(worker);
                self.operation = Some(Operation::SavingBytes);
                self.cancelling = false;
                self.message = "Saving activation bytes…".into();
            }
        } else {
            self.message = "Copy your activation bytes first.".into();
        }
        cx.notify();
    }

    fn next(&mut self) {
        let next = self
            .books
            .iter()
            .position(|book| book.state == State::Queued);
        if let Some(index) = next {
            self.operation_failed = false;
            self.operation = Some(Operation::Converting);
            self.cancelling = false;
            self.active = Some(index);
            self.books[index].state = State::Running;
            self.books[index].progress = 0.0;
            let book = &mut self.books[index];
            let args = match &book.asin {
                Some(asin) if book.path.as_os_str().is_empty() => {
                    book.detail = "Requesting download from Audible…".into();
                    vec!["fetch".into(), asin.clone()]
                }
                _ => {
                    book.detail = "Reading chapters…".into();
                    vec!["convert".into(), book.path.to_string_lossy().into()]
                }
            };
            self.worker = Some(start_worker(args, self.sender.clone()));
        } else {
            self.queue_running = false;
        }
    }

    fn convert(&mut self, _: &Convert, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = conversion_blocker(self.operation, self.connected, self.dependencies)
        {
            self.message = reason.into();
        } else if !self.books.iter().any(|book| book.state == State::Queued) {
            self.message =
                "Add an AAX file or queue a failed book again to start converting.".into();
        } else {
            self.queue_running = true;
            self.next();
        }
        cx.notify();
    }

    fn load_library(&mut self) {
        if !self.connected || !self.library_ready || self.worker.is_some() {
            return;
        }
        self.operation_failed = false;
        self.cancelling = false;
        self.operation = Some(Operation::LoadingLibrary);
        self.message = "Loading your Audible library…".into();
        self.worker = Some(start_worker(vec!["library".into()], self.sender.clone()));
    }

    fn download(&mut self, book: LibraryBook) {
        if !self.connected || self.operation == Some(Operation::LoggingOut) {
            return;
        }
        if self
            .books
            .iter()
            .any(|queued| queued.asin.as_ref() == Some(&book.asin))
        {
            return;
        }
        self.books.push(Book {
            path: PathBuf::new(),
            asin: Some(book.asin),
            title: book.title,
            state: State::Queued,
            detail: "Queued".into(),
            progress: 0.0,
            output: None,
        });
        if self.worker.is_none()
            && conversion_blocker(None, self.connected, self.dependencies).is_none()
        {
            self.queue_running = true;
            self.next();
        } else if self.operation == Some(Operation::LoadingLibrary) {
            self.queue_running = true;
        }
        self.message = "Added to the queue. It downloads to ~/Downloads, then converts.".into();
    }

    fn stop(&mut self) {
        self.browser = None;
        self.queue_running = false;
        if let Some(worker) = &self.worker {
            worker.cancel.store(true, Ordering::Relaxed);
            self.cancelling = true;
            self.message = if self.operation.is_some_and(Operation::is_login) {
                "Cancelling sign-in…".into()
            } else {
                "Stopping the current operation…".into()
            };
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.stop();
        cx.notify();
    }
}

fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    enabled: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_lg()
        .bg(rgb(0x253148))
        .text_color(rgb(0xf2f5fa))
        .text_sm()
        .when(enabled, |element| {
            element
                .cursor_pointer()
                .hover(|style| style.bg(rgb(0x344766)))
        })
        .when(!enabled, |element| {
            element.opacity(0.45).cursor(gpui::CursorStyle::Arrow)
        })
        .child(label.into())
}

impl Render for Unlock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.worker.is_some();
        let signing_in = self.operation.is_some_and(Operation::is_login);
        let waiting_for_url = self.operation == Some(Operation::LoginWaiting) && !self.cancelling;
        let blocker = conversion_blocker(self.operation, self.connected, self.dependencies);
        let queued = self
            .books
            .iter()
            .filter(|book| book.state == State::Queued)
            .count();
        let can_convert = !busy && blocker.is_none() && queued > 0;
        let can_edit_queue = self.active.is_none() && self.operation != Some(Operation::LoggingOut);
        let queued_detail = if self.operation == Some(Operation::Converting) {
            "Queued · starts after the current book"
        } else {
            blocker.unwrap_or("Ready · click Convert queue to start")
        };
        let status_color = if self.operation_failed {
            0xf0b2a5
        } else {
            0xb8d8fa
        };

        div().id("app").track_focus(&self.focus).size_full().flex().flex_col()
            .bg(rgb(0x101621)).text_color(rgb(0xe8edf5)).font_family(".AppleSystemUIFont")
            .p_5().gap_3()
            .when(!self.connected, |element| element
                .on_action(cx.listener(Self::connect))
                .on_action(cx.listener(Self::paste_bytes)))
            .when(self.connected, |element| element
                .on_action(cx.listener(Self::add_files))
                .on_action(cx.listener(Self::convert))
                .on_action(cx.listener(Self::logout)))
            .on_action(cx.listener(Self::cancel))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.add_paths(paths.paths(), cx)))
            .child(div().flex_shrink_0().flex().justify_between().items_center()
                .child(div().flex().flex_col().gap_1()
                    .child(div().text_2xl().font_weight(gpui::FontWeight::BOLD).child("Audible Unlock"))
                    .child(div().text_sm().text_color(rgb(0x94a3b8)).child(
                        if self.connected { "One lossless M4A per chapter. Your originals stay untouched." }
                        else { "Log in once to unlock your Audible audiobooks." })))
                .when(self.connected, |element| element.child(div().flex().items_center().gap_3()
                    .child(div().text_sm().text_color(rgb(0x8ae0b1)).child("Logged in"))
                    .child(button("logout", if self.operation == Some(Operation::LoggingOut) { "Logging out…" } else { "Logout" }, !busy)
                        .on_click(cx.listener(|this, _, window, cx| this.logout(&Logout, window, cx))))))
                .when(!self.connected, |element| element.child(div().text_sm().text_color(rgb(0x94a3b8))
                    .child(if signing_in { "Logging in…" } else { "Not logged in" }))))
            .child(div().flex_shrink_0().px_4().py_3().rounded_lg().bg(rgb(0x1b2b40))
                .text_sm().text_color(rgb(status_color)).child(self.message.clone()))
            .when(!self.connected, |element| element.child(
                div().id("login-view").flex_1().min_h_0().overflow_y_scroll()
                    .flex().flex_col().items_center().justify_center()
                    .child(div().w_full().max_w(px(680.)).flex_shrink_0().flex().flex_col().gap_4()
                        .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("Log in to Audible"))
                        .when(!signing_in, |element| element.child(
                            div().p_5().rounded_xl().bg(rgb(0x192231)).flex().flex_col().gap_4()
                                .child(div().text_sm().child("Choose the marketplace where your Audible library lives."))
                                .child(div().flex().gap_2().children(
                                    [("uk", "🇬🇧 UK"), ("us", "🇺🇸 US"), ("de", "🇩🇪 DE"), ("au", "🇦🇺 AU"), ("ca", "🇨🇦 CA"), ("fr", "🇫🇷 FR")]
                                        .into_iter().map(|(code, label)| {
                                            button(code, label, !busy)
                                                .bg(rgb(if self.locale == code { 0x435d88 } else { 0x253148 }))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if !this.connected && this.worker.is_none() {
                                                        this.locale = code.into();
                                                        cx.notify();
                                                    }
                                                }))
                                        })))
                                .child(button("connect", "Log in with Amazon  ⌘L", !busy).bg(rgb(0x32694e))
                                    .on_click(cx.listener(|this, _, window, cx| this.connect(&Connect, window, cx))))
                                .child(div().text_sm().text_color(rgb(0x94a3b8))
                                    .child("The sign-in window captures the login result automatically. No URL copying needed."))
                                .child(button("paste-bytes", "Use copied activation bytes  ⇧⌘V", !busy)
                                    .on_click(cx.listener(|this, _, window, cx| this.paste_bytes(&PasteBytes, window, cx))))))
            .when(signing_in, |element| element.child(
                div().flex_shrink_0().p_4().rounded_xl().bg(rgb(0x192b40)).border_1()
                    .border_color(rgb(0x43678e)).flex().flex_col().gap_3()
                    .child(div().font_weight(gpui::FontWeight::BOLD).child(
                        if self.cancelling { "Cancelling Audible sign-in…" }
                        else if self.operation == Some(Operation::LoginFinishing) { "Fetching your activation bytes…" }
                        else { "Sign in with Amazon" }))
                    .when(self.operation != Some(Operation::LoginFinishing), |element| element.child(
                        div().text_sm().flex().flex_col().gap_1()
                            .child("Complete Amazon sign-in, MFA, or CAPTCHA in the sign-in window.")
                            .child("The app captures the result automatically. Sign-in never opens an external browser.")))
                    .when(self.operation == Some(Operation::LoginFinishing), |element| element.child(
                        div().text_sm().child("The login result was received. No further action is needed.")))
                    .child(div().flex().flex_wrap().gap_2()
                        .child(button("show-login", "Show sign-in window", waiting_for_url)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.operation == Some(Operation::LoginWaiting) && !this.cancelling {
                                    this.open_login_window();
                                    cx.notify();
                                }
                            })))
                        .child(button("cancel-login", "Cancel sign-in  Esc", !self.cancelling)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(&Cancel, window, cx)))))
                    .child(div().text_xs().text_color(rgb(0x94a3b8))
                        .child("This window discards its cookies when it closes. Passwords are never saved."))))
                        .child(div().text_xs().text_color(rgb(0x94a3b8))
                            .child("Activation bytes and your optional 1Password item choice are saved on this Mac. Your Amazon password and login session are not saved.")))))
            .when(self.connected, |element| element.child(
                div().id("converter-view").flex_1().min_h_0().flex().flex_col().gap_3()
            .child(div().flex_shrink_0().flex().gap_2()
                .child(button("show-queue", format!("Queue · {}", self.books.len()), true)
                    .bg(rgb(if self.show_library { 0x253148 } else { 0x435d88 }))
                    .on_click(cx.listener(|this, _, _, cx| { this.show_library = false; cx.notify(); })))
                .child(button("show-library", "Audible library", true)
                    .bg(rgb(if self.show_library { 0x435d88 } else { 0x253148 }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_library = true;
                        if this.library.is_none() { this.load_library(); }
                        cx.notify();
                    }))))
            .child(div().flex_shrink_0().flex().flex_col().gap_2()
                .child(div().flex().items_center().gap_2()
                    .child(button("add", "Add AAX files  ⌘O", self.operation != Some(Operation::LoggingOut))
                        .on_click(cx.listener(|this, _, window, cx| this.add_files(&AddFiles, window, cx))))
                    .child(button("convert", if queued == 0 { "Convert queue  ⌘R".into() }
                        else { format!("Convert {queued} {}  ⌘R", if queued == 1 { "book" } else { "books" }) }, can_convert)
                        .bg(rgb(0x32694e))
                        .on_click(cx.listener(|this, _, window, cx| this.convert(&Convert, window, cx))))
                    .when(self.operation == Some(Operation::Converting), |element| element.child(
                        button("stop-conversion", "Stop conversion  Esc", !self.cancelling)
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(&Cancel, window, cx))))))
                .when_some(blocker, |element, reason| element.child(
                    div().text_sm().text_color(rgb(0xf0c484)).child(reason))))
            .when(self.show_library, |element| element.child(
                div().id("library").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap_2()
                    .when(!self.library_ready, |element| element.child(
                        div().p_4().rounded_xl().bg(rgb(0x192231)).text_sm()
                            .child("Log out, then log in with Amazon again to browse your library. Pasted activation bytes cannot download books.")))
                    .when(self.library_ready, |element| element.child(
                        div().flex_shrink_0().flex().items_center().justify_between().gap_3()
                            .child(div().text_sm().text_color(rgb(0x94a3b8)).child(match &self.library {
                                Some(books) => format!("{} downloadable books · saved to ~/Downloads, then converted", books.len()),
                                None if self.operation == Some(Operation::LoadingLibrary) => "Loading…".into(),
                                None => "Library not loaded yet.".into(),
                            }))
                            .child(button("refresh-library", if self.operation == Some(Operation::LoadingLibrary) { "Loading…" } else { "Refresh library" }, !busy)
                                .on_click(cx.listener(|this, _, _, cx| { this.load_library(); cx.notify(); })))))
                    .children(self.library.iter().flatten().enumerate().map(|(index, book)| {
                        let queued_state = self.books.iter().find(|queued| queued.asin.as_ref() == Some(&book.asin)).map(|queued| &queued.state);
                        let label = match queued_state {
                            Some(State::Done) => "Converted",
                            Some(State::Failed | State::Cancelled) => "In queue · retry there",
                            Some(_) => "In queue",
                            None => "Download & convert",
                        };
                        let enabled = queued_state.is_none() && self.operation != Some(Operation::LoggingOut);
                        let choice = book.clone();
                        div().flex_shrink_0().p_3().rounded_xl().bg(rgb(0x192231)).flex().items_center().justify_between().gap_3()
                            .child(cover(book.cover.clone()))
                            .child(div().flex_1().min_w_0().flex().flex_col().gap_1()
                                .child(div().child(book.title.clone()))
                                .child(div().text_sm().text_color(rgb(0x94a3b8)).child(book.detail.clone())))
                            .child(button(("download", index), label, enabled).when(enabled, |element| element.bg(rgb(0x32694e)))
                                .on_click(cx.listener(move |this, _, _, cx| { this.download(choice.clone()); cx.notify(); })))
                    }))))
            .when(!self.show_library, |element| element
            .child(div().flex_shrink_0().text_sm().text_color(rgb(0x94a3b8))
                .child(format!("Audiobooks · {} total · {queued} waiting", self.books.len())))
            .child(div().id("queue").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap_3()
                .when(self.books.is_empty(), |element| element.child(
                    div().h_32().flex_shrink_0().flex().items_center().justify_center().rounded_xl().border_1()
                        .border_color(rgb(0x344155)).text_color(rgb(0x94a3b8))
                        .child("Drop AAX files here, choose Add AAX files, or download from your Audible library.")))
                .children(self.books.iter().enumerate().map(|(index, book)| {
                    let output = book.output.clone();
                    let retry = matches!(book.state, State::Failed | State::Cancelled);
                    let artwork = book.asin.as_ref().and_then(|asin| self.library.iter().flatten().find(|entry| &entry.asin == asin)).and_then(|entry| entry.cover.clone());
                    div().flex_shrink_0().p_4().rounded_xl().bg(rgb(0x192231)).flex().flex_col().gap_2()
                        .child(div().flex().items_center().justify_between().gap_3()
                            .when(artwork.is_some(), |element| element.child(cover(artwork)))
                            .child(div().flex_1().min_w_0().child(book.title.clone()))
                            .when_some(output, |element, path| element.child(
                                button(("finder", index), "Show in Finder", true)
                                    .on_click(move |_, _, cx| cx.reveal_path(&path))))
                            .when(retry, |element| element.child(button(("retry", index), "Queue again", can_edit_queue)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.connected && this.active.is_none() && this.operation != Some(Operation::LoggingOut) {
                                        this.books[index].state = State::Queued;
                                        this.books[index].detail = "Queued".into();
                                        this.books[index].progress = 0.0;
                                        cx.notify();
                                    }
                                }))))
                            .when(book.state != State::Running, |element| element.child(
                                button(("remove", index), "Remove", can_edit_queue)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.connected && this.active.is_none() && this.operation != Some(Operation::LoggingOut) { this.books.remove(index); cx.notify(); }
                                    })))))
                        .child(div().text_sm().text_color(rgb(if retry { 0xf0b2a5 } else { 0x94a3b8 }))
                            .child(if book.state == State::Queued { queued_detail.to_owned() } else { book.detail.clone() }))
                        .when(matches!(book.state, State::Running | State::Done), |element| element.child(
                            div().h(px(4.)).w_full().rounded_full().bg(rgb(0x253148))
                                .child(div().h_full().w(gpui::relative(book.progress)).rounded_full().bg(rgb(0x8ae0b1)))))
                })))
            .child(div().flex_shrink_0().text_xs().text_color(rgb(0x94a3b8))
                .child("Output folders are saved next to the source. Existing output folders are never replaced."))) ))
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--print-login-helper") {
        println!("{}", login_browser::helper_path().display());
        return;
    }
    let application = Application::new();
    if std::env::args().any(|arg| arg == "--check-text") {
        let text = application.text_system();
        assert!(
            !text.all_font_names().is_empty(),
            "GPUI has no fonts; enable font-kit."
        );
        let font = text.resolve_font(&gpui::font(".AppleSystemUIFont"));
        assert!(text.cap_height(font, px(16.)) > px(0.));
        println!("Native text rendering is enabled and the system font has valid metrics.");
        return;
    }
    application.run(|cx: &mut App| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus(vec![
            gpui::Menu {
                name: "Audible Unlock".into(),
                items: vec![
                    gpui::MenuItem::action("Log in to Audible", Connect),
                    gpui::MenuItem::action("Logout", Logout),
                    gpui::MenuItem::action("Paste activation bytes", PasteBytes),
                    gpui::MenuItem::separator(),
                    gpui::MenuItem::action("Quit Audible Unlock", Quit),
                ],
            },
            gpui::Menu {
                name: "File".into(),
                items: vec![
                    gpui::MenuItem::action("Add AAX files…", AddFiles),
                    gpui::MenuItem::action("Convert queue", Convert),
                    gpui::MenuItem::action("Cancel", Cancel),
                ],
            },
        ]);
        cx.bind_keys([
            KeyBinding::new("cmd-o", AddFiles, None),
            KeyBinding::new("cmd-l", Connect, None),
            KeyBinding::new("cmd-alt-l", Logout, None),
            KeyBinding::new("cmd-shift-v", PasteBytes, None),
            KeyBinding::new("cmd-r", Convert, None),
            KeyBinding::new("escape", Cancel, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_window_closed(|cx| cx.quit()).detach();
        let bounds = Bounds::centered(None, size(px(900.), px(740.)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Audible Unlock".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.), px(620.))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Unlock::new(window, cx)),
        )
        .expect("Unable to open the Audible Unlock window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_keys_do_not_hide_pending_sign_in() {
        for operation in [
            Operation::LoginOpening,
            Operation::LoginWaiting,
            Operation::LoginFinishing,
        ] {
            assert_eq!(
                conversion_blocker(Some(operation), true, true),
                Some("Finish or cancel sign-in before converting.")
            );
        }
    }

    #[test]
    fn conversion_needs_idle_ready_setup() {
        assert_eq!(conversion_blocker(None, true, true), None);
        assert!(conversion_blocker(None, false, true).is_some());
        assert!(conversion_blocker(None, true, false).is_some());
        for operation in [
            Operation::Checking,
            Operation::SavingBytes,
            Operation::LoggingOut,
            Operation::Converting,
        ] {
            assert!(conversion_blocker(Some(operation), true, true).is_some());
        }
    }

    #[test]
    fn library_rows_skip_invalid_entries_and_format_length() {
        let books = library_books(&json!([
            {"asin": "B000000001", "title": "Sourcery", "authors": "Terry Pratchett", "minutes": 425, "cover": "/tmp/B000000001.jpg"},
            {"asin": "B000000002", "title": "Anonymous", "authors": "", "minutes": 59},
            {"title": "Missing identifier"},
        ]));
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].detail, "Terry Pratchett · 7 h 05 min");
        assert_eq!(books[1].detail, "0 h 59 min");
        assert_eq!(books[0].cover, Some(PathBuf::from("/tmp/B000000001.jpg")));
        assert_eq!(books[1].cover, None);
    }

    #[test]
    fn backend_status_round_trip() {
        let (sender, receiver) = mpsc::channel();
        let _worker = start_worker(vec!["status".into()], sender);
        let status = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(status["kind"], "status");
        assert!(status["connected"].is_boolean());
        assert!(status["library"].is_boolean());
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap()["kind"],
            "finished"
        );
    }

    #[test]
    fn browser_login_can_cancel_without_credentials() {
        let (sender, receiver) = mpsc::channel();
        let worker = start_worker(vec!["login".into(), "uk".into()], sender);
        let event = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(event["kind"], "login_url");
        assert!(
            event["url"]
                .as_str()
                .unwrap()
                .starts_with("https://www.amazon.co.uk/")
        );
        worker.cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap()["kind"],
            "cancelled"
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap()["kind"],
            "finished"
        );
    }
}
