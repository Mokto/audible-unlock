use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

macro_rules! choice {
    ($name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            pub fn parse(value: &str) -> Option<Self> {
                match value { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

choice!(Format { Chapters = "chapters", M4b = "m4b" });
choice!(Layout { Flat = "flat", Author = "author" });
choice!(Appearance { System = "system", Light = "light", Dark = "dark" });
choice!(Sort { Recent = "recent", Title = "title", Author = "author", Length = "length" });

#[derive(Clone, Debug)]
pub struct Settings {
    pub locale: String,
    pub format: Format,
    pub layout: Layout,
    pub output_dir: Option<PathBuf>,
    pub trash_source: bool,
    pub appearance: Appearance,
    pub check_updates: bool,
    pub onboarded: bool,
    pub grid: bool,
    pub sort: Sort,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            locale: "uk".into(),
            format: Format::Chapters,
            layout: Layout::Flat,
            output_dir: None,
            trash_source: false,
            appearance: Appearance::System,
            check_updates: true,
            onboarded: false,
            grid: false,
            sort: Sort::Recent,
        }
    }
}

fn settings_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join("Library/Application Support/Earshelf/settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let value = std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .unwrap_or(Value::Null);
        Self::from_json(&value)
    }

    pub fn from_json(value: &Value) -> Self {
        let default = Self::default();
        let choice = |key: &str| value[key].as_str();
        Self {
            locale: choice("locale")
                .filter(|locale| ["uk", "us", "de", "au", "ca", "fr"].contains(locale))
                .map_or(default.locale, str::to_owned),
            format: choice("format")
                .and_then(Format::parse)
                .unwrap_or(default.format),
            layout: choice("layout")
                .and_then(Layout::parse)
                .unwrap_or(default.layout),
            output_dir: choice("output_dir")
                .filter(|dir| Path::new(dir).is_absolute())
                .map(PathBuf::from),
            trash_source: value["trash_source"].as_bool().unwrap_or(false),
            appearance: choice("appearance")
                .and_then(Appearance::parse)
                .unwrap_or(default.appearance),
            check_updates: value["check_updates"].as_bool().unwrap_or(true),
            onboarded: value["onboarded"].as_bool().unwrap_or(false),
            grid: value["grid"].as_bool().unwrap_or(false),
            sort: choice("sort").and_then(Sort::parse).unwrap_or(default.sort),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "locale": self.locale, "format": self.format.as_str(), "layout": self.layout.as_str(),
            "output_dir": self.output_dir, "trash_source": self.trash_source,
            "appearance": self.appearance.as_str(), "check_updates": self.check_updates,
            "onboarded": self.onboarded, "grid": self.grid, "sort": self.sort.as_str(),
        })
    }

    pub fn save(&self) {
        let path = settings_path();
        let temporary = path.with_extension("json.tmp");
        if std::fs::create_dir_all(path.parent().unwrap()).is_ok()
            && std::fs::write(&temporary, self.to_json().to_string()).is_ok()
        {
            let _ = std::fs::rename(temporary, path);
        }
    }

    /// The conversion options sent to the backend with each fetch or convert.
    pub fn options_json(&self) -> String {
        json!({
            "format": self.format.as_str(), "layout": self.layout.as_str(),
            "output_dir": self.output_dir,
        })
        .to_string()
    }

    /// Folders that must have room for a download and its output.
    pub fn work_dirs(&self) -> Vec<PathBuf> {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
        let mut dirs = vec![home.join("Downloads")];
        dirs.extend(self.output_dir.clone());
        dirs
    }
}

pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut path = path;
    while !path.exists() {
        path = path.parent()?;
    }
    let path = std::ffi::CString::new(path.to_str()?).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(path.as_ptr(), &mut stats) } == 0)
        .then(|| stats.f_bavail as u64 * stats.f_frsize)
}

pub fn size_text(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

/// Moves a finished download (and its voucher) to the Trash via the Swift helper.
pub fn trash(paths: Vec<PathBuf>) {
    let _ = Command::new(crate::login_browser::helper_path())
        .arg("--trash")
        .args(paths)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Posts a macOS notification. AppleScript is used because notifications from a child process of an
/// ad-hoc signed app are refused by UserNotifications; arguments are passed as data, never as script text.
pub fn notify(title: String, body: String) {
    std::thread::spawn(move || {
        let _ = Command::new("/usr/bin/osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 2 of argv) with title (item 1 of argv)",
                "-e",
                "end run",
            ])
            .args([&title, &body])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

#[allow(unexpected_cfgs)] // the objc 0.2 macros probe a cfg that doesn't exist in this crate
pub fn set_dock_badge(label: &str) {
    use cocoa::{
        appkit::NSApp,
        base::{id, nil},
        foundation::NSString,
    };
    use objc::{msg_send, sel, sel_impl};
    unsafe {
        let tile: id = msg_send![NSApp(), dockTile];
        let text = NSString::alloc(nil).init_str(label);
        let _: () = msg_send![tile, setBadgeLabel: text];
    }
}

/// `Some((version, url))` when GitHub has a newer release than `current`.
pub fn newer_release(current: &str, body: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(body).ok()?;
    let tag = value["tag_name"].as_str()?.trim_start_matches('v');
    let url = value["html_url"].as_str()?;
    let parse = |text: &str| -> Option<Vec<u64>> {
        text.split('.').map(|part| part.parse().ok()).collect()
    };
    (url.starts_with("https://github.com/Mokto/earshelf/") && parse(tag)? > parse(current)?)
        .then(|| (tag.to_owned(), url.to_owned()))
}

pub fn check_for_update(current: &'static str, found: std::sync::mpsc::Sender<(String, String)>) {
    std::thread::spawn(move || {
        let output = Command::new("/usr/bin/curl")
            .args([
                "-fsSL",
                "-m",
                "10",
                "-H",
                "Accept: application/vnd.github+json",
                "https://api.github.com/repos/Mokto/earshelf/releases/latest",
            ])
            .stderr(Stdio::null())
            .output();
        if let Ok(output) = output
            && let Some(update) = newer_release(current, &String::from_utf8_lossy(&output.stdout))
        {
            let _ = found.send(update);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_reject_bad_values() {
        let settings = Settings {
            format: Format::M4b,
            layout: Layout::Author,
            output_dir: Some("/Volumes/Books".into()),
            sort: Sort::Length,
            locale: "de".into(),
            ..Settings::default()
        };
        let loaded = Settings::from_json(&settings.to_json());
        assert_eq!(loaded.to_json(), settings.to_json());
        let bad = Settings::from_json(&json!({
            "locale": "../x", "format": "mp3", "output_dir": "relative/path", "sort": 5,
        }));
        assert_eq!(bad.to_json(), Settings::default().to_json());
        assert_eq!(
            serde_json::from_str::<Value>(&settings.options_json()).unwrap(),
            json!({"format": "m4b", "layout": "author", "output_dir": "/Volumes/Books"})
        );
    }

    #[test]
    fn only_newer_releases_from_this_repository_are_offered() {
        let release = |tag: &str, url: &str| json!({"tag_name": tag, "html_url": url}).to_string();
        let url = "https://github.com/Mokto/earshelf/releases/tag/v0.10.0";
        assert_eq!(
            newer_release("0.9.0", &release("v0.10.0", url)),
            Some(("0.10.0".into(), url.into()))
        );
        assert_eq!(newer_release("0.10.0", &release("v0.10.0", url)), None);
        assert_eq!(
            newer_release("0.2.0", &release("v0.10.0", "https://evil.example/")),
            None
        );
        assert_eq!(newer_release("0.2.0", &release("nightly", url)), None);
        assert_eq!(newer_release("0.2.0", "not json"), None);
    }

    #[test]
    fn sizes_and_free_space() {
        assert_eq!(size_text(310_000_000), "310 MB");
        assert_eq!(size_text(1_250_000_000), "1.2 GB");
        assert!(free_bytes(Path::new("/definitely/not/created")).unwrap() > 0);
    }
}
