use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

pub fn helper_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(directory) = exe.parent()
    {
        let bundled = directory.join("audible-login");
        if bundled.is_file() {
            return bundled;
        }
    }
    PathBuf::from(env!("AUDIBLE_LOGIN_HELPER"))
}

pub struct LoginBrowser {
    child: Option<Child>,
    stdin: ChildStdin,
}

impl LoginBrowser {
    pub fn show(&mut self) {
        let _ = writeln!(self.stdin, "{}", json!({"kind":"show"}));
        let _ = self.stdin.flush();
    }

    pub fn open(url: &str, session: u64, events: mpsc::Sender<Value>) -> std::io::Result<Self> {
        let mut child = Command::new(helper_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut browser = Self {
            child: Some(child),
            stdin,
        };
        writeln!(browser.stdin, "{}", json!({"url":url}))?;
        browser.stdin.flush()?;
        thread::spawn(move || {
            let mut terminal = false;
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut event) = serde_json::from_str::<Value>(&line)
                    && event.is_object()
                {
                    terminal |= matches!(
                        event["kind"].as_str(),
                        Some("webview_redirect" | "webview_closed" | "webview_error")
                    );
                    event["session"] = json!(session);
                    if events.send(event).is_err() {
                        return;
                    }
                }
            }
            if !terminal {
                let _ = events.send(json!({"kind":"webview_error", "session":session}));
            }
        });
        Ok(browser)
    }
}

impl Drop for LoginBrowser {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "{}", json!({"kind":"cancel"}));
        let _ = self.stdin.flush();
        if let Some(mut child) = self.child.take() {
            thread::spawn(move || {
                let started = Instant::now();
                while matches!(child.try_wait(), Ok(None))
                    && started.elapsed() < Duration::from_secs(2)
                {
                    thread::sleep(Duration::from_millis(25));
                }
                if matches!(child.try_wait(), Ok(None)) {
                    let _ = child.kill();
                }
                let _ = child.wait();
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_redirect_validation_and_private_storage() {
        let output = Command::new(helper_path())
            .arg("--self-test")
            .output()
            .unwrap();
        assert!(output.status.success(), "Native self-check failed");
    }

    #[test]
    fn webkit_captures_redirect_before_loading_error_page() {
        check_webkit("--smoke-test", "webview_redirect");
    }

    #[test]
    fn webkit_autofill_handles_passwords_and_totp_and_refuses_unsafe_fields() {
        check_webkit("--autofill-smoke-test", "webview_autofill_checked");
    }

    fn check_webkit(argument: &str, expected_kind: &str) {
        let mut child = Command::new(helper_path())
            .arg(argument)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{}", json!({"url":"https://www.amazon.co.uk/ap/signin?openid.return_to=https%3A%2F%2Fwww.amazon.co.uk%2Fap%2Fmaplanding"})).unwrap();
        stdin.flush().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    let _ = sender.send(value);
                }
            }
        });
        let captured = loop {
            match receiver.recv_timeout(Duration::from_secs(20)) {
                Ok(event)
                    if matches!(
                        event["kind"].as_str(),
                        Some("webview_opened" | "webview_ready")
                    ) =>
                {
                    continue;
                }
                Ok(event) => {
                    break event["kind"] == expected_kind
                        && (expected_kind != "webview_redirect"
                            || event["value"]
                                .as_str()
                                .is_some_and(|value| value.contains("synthetic-test-code")));
                }
                Err(_) => break false,
            }
        };
        let _ = writeln!(stdin, "{}", json!({"kind":"cancel"}));
        let _ = stdin.flush();
        let started = Instant::now();
        while matches!(child.try_wait(), Ok(None)) && started.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            captured,
            "WebKit offline integration check failed: {argument}"
        );
    }
}
