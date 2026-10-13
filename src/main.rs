mod login_browser;
mod settings;
mod theme;
mod views;

use gpui::{
    App, AppContext, Application, Bounds, ClipboardItem, Context, ExternalPaths, FocusHandle,
    KeyBinding, KeyDownEvent, PathPromptOptions, Pixels, Point, PromptLevel, ScrollStrategy,
    UniformListScrollHandle, Window, WindowAppearance, WindowBounds, WindowOptions, actions, px,
    size,
};
use serde_json::{Value, json};
use settings::{Appearance, Format, Settings, Sort};
use std::{
    collections::HashSet,
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
use theme::Theme;

actions!(
    earshelf,
    [
        AddFiles,
        Connect,
        Logout,
        PasteBytes,
        Convert,
        Cancel,
        Quit,
        ShowLibrary,
        ShowQueue,
        ShowSettings,
        FocusSearch,
        SelectAll
    ]
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
                    .join("Library/Caches/earshelf/python"),
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

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Library,
    Queue,
    Settings,
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

#[derive(Clone, Default)]
struct LibraryBook {
    asin: String,
    title: String,
    authors: String,
    narrators: String,
    series: String,
    sequence: String,
    purchased: String,
    minutes: u64,
    kbps: Option<u64>,
    bytes: Option<u64>,
    cover: Option<PathBuf>,
    output: Option<PathBuf>,
    source: Option<PathBuf>,
}

impl LibraryBook {
    fn length(&self) -> String {
        format!("{} h {:02} min", self.minutes / 60, self.minutes % 60)
    }

    fn matches(&self, query: &str) -> bool {
        [&self.title, &self.authors, &self.narrators, &self.series]
            .iter()
            .any(|field| field.to_lowercase().contains(query))
    }
}

fn library_books(value: &Value) -> Vec<LibraryBook> {
    let text = |book: &Value, key: &str| book[key].as_str().unwrap_or_default().to_owned();
    let path = |book: &Value, key: &str| book[key].as_str().map(PathBuf::from);
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|book| {
            Some(LibraryBook {
                asin: book["asin"].as_str()?.into(),
                title: book["title"].as_str()?.into(),
                authors: text(book, "authors"),
                narrators: text(book, "narrators"),
                series: text(book, "series"),
                sequence: text(book, "sequence"),
                purchased: text(book, "purchased"),
                minutes: book["minutes"].as_u64().unwrap_or(0),
                kbps: book["kbps"].as_u64(),
                bytes: book["bytes"].as_u64(),
                cover: path(book, "cover"),
                output: path(book, "output"),
                source: path(book, "source"),
            })
        })
        .collect()
}

#[derive(Clone)]
enum MenuAction {
    Reveal(PathBuf),
    Download(String),
    Retry(usize),
    Remove(usize),
    CopyTitle(String),
}

#[derive(Clone)]
struct ContextMenu {
    position: Point<Pixels>,
    items: Vec<(&'static str, MenuAction)>,
}

struct Earshelf {
    focus: FocusHandle,
    search_focus: FocusHandle,
    settings: Settings,
    section: Section,
    books: Vec<Book>,
    library: Option<Vec<LibraryBook>>,
    library_ready: bool,
    library_requested: bool,
    library_error: Option<String>,
    query: String,
    selected: HashSet<String>,
    cursor: Option<usize>,
    list_scroll: UniformListScrollHandle,
    menu: Option<ContextMenu>,
    update: Option<(String, String)>,
    updates: mpsc::Receiver<(String, String)>,
    confirming: bool,
    advanced: bool,
    os_version: String,
    badge: String,
    connected: bool,
    dependencies: bool,
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

impl Earshelf {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (sender, events) = mpsc::channel();
        let (found, updates) = mpsc::channel();
        let settings = Settings::load();
        if settings.check_updates {
            settings::check_for_update(env!("CARGO_PKG_VERSION"), found);
        }
        let focus = cx.focus_handle();
        focus.focus(window);
        let this = cx.entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.should_close(window, cx))
        });
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        cx.on_app_quit(|this, cx| {
            this.queue_running = false;
            this.browser = None;
            if let Some(worker) = &this.worker {
                worker.cancel.store(true, Ordering::Relaxed);
            }
            settings::set_dock_badge("");
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
            search_focus: cx.focus_handle(),
            focus,
            settings,
            section: Section::Library,
            books: vec![],
            library: None,
            library_ready: false,
            library_requested: false,
            library_error: None,
            query: String::new(),
            selected: HashSet::new(),
            cursor: None,
            list_scroll: UniformListScrollHandle::new(),
            menu: None,
            update: None,
            updates,
            confirming: false,
            advanced: false,
            os_version: Command::new("sw_vers")
                .arg("-productVersion")
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
                .unwrap_or_default(),
            badge: String::new(),
            connected: false,
            dependencies: false,
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

    fn theme(&self, window: &Window) -> Theme {
        match self.settings.appearance {
            Appearance::Light => Theme::LIGHT,
            Appearance::Dark => Theme::DARK,
            Appearance::System => match window.appearance() {
                WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::LIGHT,
                _ => Theme::DARK,
            },
        }
    }

    fn save_settings(&self) {
        self.settings.save();
    }

    fn working(&self) -> bool {
        self.operation == Some(Operation::Converting)
            || self.books.iter().any(|book| book.state == State::Running)
    }

    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.working() {
            return true;
        }
        self.confirm_quit(window, cx);
        false
    }

    fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.working() {
            cx.quit();
            return;
        }
        if self.confirming {
            return;
        }
        self.confirming = true;
        let title = self
            .active
            .map(|index| self.books[index].title.clone())
            .unwrap_or_default();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Stop {title} and quit Earshelf?"),
            Some("A download or conversion is still running. Partial files are discarded; finished books and your original files are unchanged."),
            &["Stop and quit", "Keep working"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            let quit = answer.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                this.confirming = false;
                if quit {
                    cx.quit();
                }
            });
        })
        .detach();
    }

    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_quit(window, cx);
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        if let Ok(update) = self.updates.try_recv() {
            self.update = Some(update);
            cx.notify();
        }
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
                        "Ready.".into()
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
                    self.section = Section::Library;
                    self.message = "Logged in.".into();
                }
                "logged_out" => {
                    self.connected = false;
                    self.books.clear();
                    self.library = None;
                    self.library_ready = false;
                    self.library_requested = false;
                    self.library_error = None;
                    self.query.clear();
                    self.selected.clear();
                    self.cursor = None;
                    self.menu = None;
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
                    self.library_error = None;
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
                        self.books[index].detail = if event["format"] == "m4b" {
                            format!("Writing one M4B with {} chapters…", event["total"])
                        } else {
                            format!("0 / {} chapters", event["total"])
                        };
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
                "done" => self.finish_book(&event, cx),
                "warning" => {
                    self.message = text("message");
                    if self.operation == Some(Operation::LoginFinishing) {
                        self.operation = Some(Operation::LoginWaiting);
                    }
                }
                "error" => {
                    self.browser = None;
                    self.operation_failed = true;
                    if let Some(index) = self.active {
                        self.books[index].state = State::Failed;
                        self.books[index].detail = text("message");
                        if cx.active_window().is_none() {
                            settings::notify(
                                "Earshelf couldn't finish a book".into(),
                                self.books[index].title.clone(),
                            );
                        }
                    } else if self.operation == Some(Operation::LoadingLibrary) {
                        self.library_error = Some(text("message"));
                    } else {
                        self.message = text("message");
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
                    if self.worker.is_none()
                        && self.connected
                        && self.library_ready
                        && !self.library_requested
                    {
                        self.load_library();
                    }
                }
                _ => {}
            }
        }
        let remaining = self
            .books
            .iter()
            .filter(|book| matches!(book.state, State::Queued | State::Running))
            .count();
        let badge = if remaining == 0 {
            String::new()
        } else {
            remaining.to_string()
        };
        if badge != self.badge {
            settings::set_dock_badge(&badge);
            self.badge = badge;
        }
        cx.notify();
    }

    fn finish_book(&mut self, event: &Value, cx: &mut Context<Self>) {
        let Some(index) = self.active else { return };
        let output = PathBuf::from(event["output"].as_str().unwrap_or_default());
        let source = PathBuf::from(event["source"].as_str().unwrap_or_default());
        let single = event["format"] == "m4b";
        let book = &mut self.books[index];
        book.state = State::Done;
        book.progress = 1.0;
        book.output = Some(output.clone());
        book.detail = if single {
            "Complete · one lossless M4B".into()
        } else {
            "Complete · lossless M4A".into()
        };
        let mut trashed = false;
        if self.settings.trash_source && source.is_file() {
            settings::trash(vec![source.clone(), source.with_extension("voucher")]);
            trashed = !source.exists();
        }
        let title = book.title.clone();
        self.message = if trashed {
            format!("Finished {title}. The original was moved to the Trash.")
        } else {
            format!("Finished {title}. The original is unchanged.")
        };
        if let Some(asin) = book.asin.clone()
            && let Some(entry) = self.library.iter_mut().flatten().find(|b| b.asin == asin)
        {
            entry.output = Some(output);
            entry.source = (!trashed).then_some(source);
        }
        if cx.active_window().is_none() {
            settings::notify(
                format!("Finished {title}"),
                if single {
                    "Ready as one M4B with chapters.".into()
                } else {
                    "Ready as chapter files.".into()
                },
            );
        }
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
            self.section = Section::Queue;
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

    fn choose_output_folder(&mut self, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose the folder for converted audiobooks".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picker.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    this.settings.output_dir = Some(path);
                    this.save_settings();
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
            vec!["login".into(), self.settings.locale.clone()],
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
            let options = self.settings.options_json();
            let book = &mut self.books[index];
            let mut args = match &book.asin {
                Some(asin) if book.path.as_os_str().is_empty() => {
                    book.detail = "Requesting download from Audible…".into();
                    vec!["fetch".into(), asin.clone()]
                }
                _ => {
                    book.detail = "Reading chapters…".into();
                    vec!["convert".into(), book.path.to_string_lossy().into()]
                }
            };
            args.push(options);
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
                "Nothing is waiting. Add an AAX file or pick books from the library.".into();
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
        self.library_requested = true;
        self.library_error = None;
        self.operation_failed = false;
        self.cancelling = false;
        self.operation = Some(Operation::LoadingLibrary);
        self.message = "Loading your Audible library…".into();
        self.worker = Some(start_worker(vec!["library".into()], self.sender.clone()));
    }

    fn queue_index(&self, asin: &str) -> Option<usize> {
        self.books
            .iter()
            .position(|queued| queued.asin.as_deref() == Some(asin))
    }

    /// Queues library books and starts working if idle. Skips books already queued or converted.
    /// Stops early, with a message, when the Mac would run out of disk space.
    fn download_books(&mut self, asins: &[String]) {
        if !self.connected || self.operation == Some(Operation::LoggingOut) {
            return;
        }
        let mut budget = self
            .settings
            .work_dirs()
            .iter()
            .filter_map(|dir| settings::free_bytes(dir))
            .min()
            .map(|free| free.saturating_sub(500_000_000));
        let mut added = 0;
        let mut short = false;
        for asin in asins {
            let Some(book) = self
                .library
                .iter()
                .flatten()
                .find(|book| &book.asin == asin)
                .cloned()
            else {
                continue;
            };
            if self.queue_index(asin).is_some() || book.output.is_some() {
                continue;
            }
            let estimate = book.bytes.unwrap_or(400_000_000);
            let need = if book.source.is_some() {
                estimate
            } else {
                estimate * 2
            };
            if let Some(left) = budget.as_mut() {
                if need > *left {
                    short = true;
                    break;
                }
                *left -= need;
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
            added += 1;
        }
        self.message = if short {
            "Not enough free disk space for every selected book. The rest were not queued; free some space or choose another output folder in Settings.".into()
        } else if added > 0 {
            format!(
                "Added {added} {} to the queue. Downloads go to ~/Downloads, then convert.",
                if added == 1 { "book" } else { "books" }
            )
        } else {
            "Nothing to add: those books are already queued or converted.".into()
        };
        self.kick();
    }

    fn kick(&mut self) {
        if self.worker.is_none()
            && conversion_blocker(None, self.connected, self.dependencies).is_none()
            && self.books.iter().any(|book| book.state == State::Queued)
        {
            self.queue_running = true;
            self.next();
        } else if self.operation == Some(Operation::LoadingLibrary) {
            self.queue_running = true;
        }
    }

    fn retry(&mut self, index: usize) {
        if let Some(book) = self.books.get_mut(index)
            && matches!(book.state, State::Failed | State::Cancelled)
            && self.connected
            && self.operation != Some(Operation::LoggingOut)
        {
            book.state = State::Queued;
            book.detail = "Queued".into();
            book.progress = 0.0;
            self.kick();
        }
    }

    fn remove(&mut self, index: usize) {
        if self.connected
            && self.active.is_none()
            && self.operation != Some(Operation::LoggingOut)
            && index < self.books.len()
        {
            self.books.remove(index);
            self.cursor = None;
        }
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

    fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
        } else if self.search_focus.is_focused(window)
            || (!self.query.is_empty() && !self.working())
        {
            self.query.clear();
            self.cursor = None;
            self.focus.focus(window);
        } else {
            self.stop();
        }
        cx.notify();
    }

    fn diagnostics(&self, detail: &str) -> String {
        format!(
            "Earshelf {}\nmacOS {} ({})\nMarketplace: {}\nOutput: {} / {}\nProblem: {detail}",
            env!("CARGO_PKG_VERSION"),
            self.os_version,
            std::env::consts::ARCH,
            self.settings.locale,
            self.settings.format.as_str(),
            self.settings.layout.as_str(),
        )
    }

    /// Indices into `library` that match the search, in the chosen order.
    fn visible(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        let books = self.library.as_deref().unwrap_or_default();
        let mut shown: Vec<usize> = (0..books.len())
            .filter(|&index| query.is_empty() || books[index].matches(&query))
            .collect();
        match self.settings.sort {
            Sort::Recent => {}
            Sort::Title => shown.sort_by_key(|&i| books[i].title.to_lowercase()),
            Sort::Author => shown.sort_by_key(|&i| {
                (
                    books[i].authors.to_lowercase(),
                    books[i].title.to_lowercase(),
                )
            }),
            Sort::Length => shown.sort_by_key(|&i| std::cmp::Reverse(books[i].minutes)),
        }
        shown
    }

    fn list_len(&self) -> usize {
        match self.section {
            Section::Library => self.visible().len(),
            Section::Queue => self.books.len(),
            Section::Settings => 0,
        }
    }

    fn move_cursor(&mut self, delta: isize, window: &Window) {
        let len = self.list_len();
        if len == 0 {
            return;
        }
        let next = match self.cursor {
            None => 0,
            Some(current) => (current as isize + delta).clamp(0, len as isize - 1) as usize,
        };
        self.cursor = Some(next);
        let columns = views::columns(window, self.settings.grid);
        let row = if self.section == Section::Library && self.settings.grid {
            next / columns
        } else {
            next
        };
        self.list_scroll.scroll_to_item(row, ScrollStrategy::Center);
    }

    fn activate(&mut self, cx: &mut Context<Self>) {
        let Some(cursor) = self.cursor else { return };
        match self.section {
            Section::Library => {
                if let Some(&index) = self.visible().get(cursor) {
                    let asin = self.library.as_ref().unwrap()[index].asin.clone();
                    self.activate_book(&asin, cx);
                }
            }
            Section::Queue => match self.books.get(cursor) {
                Some(book) if book.output.is_some() => {
                    cx.reveal_path(book.output.as_ref().unwrap())
                }
                Some(_) => self.retry(cursor),
                None => {}
            },
            Section::Settings => {}
        }
    }

    fn activate_book(&mut self, asin: &str, cx: &mut Context<Self>) {
        match self.row_primary(asin) {
            views::Primary::Fetch => self.download_books(&[asin.to_owned()]),
            views::Primary::Reveal(path) => cx.reveal_path(&path),
            views::Primary::Retry(index) => self.retry(index),
            views::Primary::None => {}
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.connected || self.section == Section::Settings {
            return;
        }
        let typing = self.search_focus.is_focused(window);
        let grid = self.section == Section::Library && self.settings.grid;
        let columns = views::columns(window, grid) as isize;
        match event.keystroke.key.as_str() {
            "down" => self.move_cursor(if grid { columns } else { 1 }, window),
            "up" => self.move_cursor(if grid { -columns } else { -1 }, window),
            "right" if grid && !typing => self.move_cursor(1, window),
            "left" if grid && !typing => self.move_cursor(-1, window),
            "enter" => {
                self.focus.focus(window);
                self.activate(cx);
            }
            "space" if !typing && self.section == Section::Library => {
                if let Some(&index) = self
                    .cursor
                    .and_then(|c| self.visible().get(c).copied())
                    .as_ref()
                {
                    let asin = self.library.as_ref().unwrap()[index].asin.clone();
                    if !self.selected.remove(&asin) {
                        self.selected.insert(asin);
                    }
                }
            }
            "backspace" | "delete" if !typing && self.section == Section::Queue => {
                if let Some(cursor) = self.cursor
                    && self
                        .books
                        .get(cursor)
                        .is_some_and(|b| b.state != State::Running)
                {
                    self.remove(cursor);
                }
            }
            _ => return,
        }
        cx.notify();
    }

    fn search_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let before = self.query.len();
        if keystroke.modifiers.platform {
            if keystroke.key == "v"
                && let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
            {
                self.query.push_str(&text.replace(['\n', '\r'], " "));
            }
        } else if keystroke.key == "backspace" {
            self.query.pop();
        } else if !keystroke.modifiers.control
            && let Some(text) = &keystroke.key_char
            && !text.chars().any(char::is_control)
        {
            self.query.push_str(text);
        } else {
            return;
        }
        if self.query.len() != before || keystroke.key == "backspace" {
            self.cursor = None;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn run_menu(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        self.menu = None;
        match action {
            MenuAction::Reveal(path) => cx.reveal_path(&path),
            MenuAction::Download(asin) => self.download_books(&[asin]),
            MenuAction::Retry(index) => self.retry(index),
            MenuAction::Remove(index) => self.remove(index),
            MenuAction::CopyTitle(title) => cx.write_to_clipboard(ClipboardItem::new_string(title)),
        }
        cx.notify();
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
                name: "Earshelf".into(),
                items: vec![
                    gpui::MenuItem::action("Log in to Audible", Connect),
                    gpui::MenuItem::action("Logout", Logout),
                    gpui::MenuItem::action("Paste activation bytes", PasteBytes),
                    gpui::MenuItem::separator(),
                    gpui::MenuItem::action("Settings…", ShowSettings),
                    gpui::MenuItem::separator(),
                    gpui::MenuItem::action("Quit Earshelf", Quit),
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
            gpui::Menu {
                name: "View".into(),
                items: vec![
                    gpui::MenuItem::action("Library", ShowLibrary),
                    gpui::MenuItem::action("Queue", ShowQueue),
                    gpui::MenuItem::action("Find in library", FocusSearch),
                    gpui::MenuItem::action("Select all books", SelectAll),
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
            KeyBinding::new("cmd-1", ShowLibrary, None),
            KeyBinding::new("cmd-2", ShowQueue, None),
            KeyBinding::new("cmd-,", ShowSettings, None),
            KeyBinding::new("cmd-f", FocusSearch, None),
            KeyBinding::new("cmd-a", SelectAll, None),
        ]);
        cx.on_window_closed(|cx| cx.quit()).detach();
        let bounds = Bounds::centered(None, size(px(1040.), px(780.)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Earshelf".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(840.), px(600.))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Earshelf::new(window, cx)),
        )
        .expect("Unable to open the Earshelf window");
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
    fn library_rows_skip_invalid_entries_and_keep_details() {
        let books = library_books(&json!([
            {"asin": "B000000001", "title": "Sourcery", "authors": "Terry Pratchett", "narrators": "Nigel Planer",
             "series": "Discworld", "sequence": "5", "minutes": 425, "kbps": 64, "bytes": 204000000,
             "cover": "/tmp/B000000001.jpg", "output": "/tmp/Sourcery"},
            {"asin": "B000000002", "title": "Anonymous", "minutes": 59},
            {"title": "Missing identifier"},
        ]));
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].length(), "7 h 05 min");
        assert_eq!(books[1].length(), "0 h 59 min");
        assert_eq!(books[0].cover, Some(PathBuf::from("/tmp/B000000001.jpg")));
        assert_eq!(books[0].output, Some(PathBuf::from("/tmp/Sourcery")));
        assert_eq!(
            (
                books[1].cover.clone(),
                books[1].output.clone(),
                books[1].bytes
            ),
            (None, None, None)
        );
        assert!(
            books[0].matches("planer")
                && books[0].matches("discworld")
                && !books[0].matches("tolkien")
        );
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
