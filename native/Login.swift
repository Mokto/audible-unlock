import AppKit
import Foundation
import WebKit

struct RedirectTarget {
    let url: URL
    let amazonDomain: String

    init?(loginURL: URL) {
        guard loginURL.scheme == "https", loginURL.user == nil, loginURL.password == nil,
              let host = loginURL.host,
              ["www.amazon.co.uk", "www.amazon.com", "www.amazon.de", "www.amazon.com.au", "www.amazon.ca", "www.amazon.fr"].contains(host),
              let parts = URLComponents(url: loginURL, resolvingAgainstBaseURL: false) else { return nil }
        let targets = (parts.queryItems ?? []).filter { $0.name == "openid.return_to" }
        guard targets.count == 1, let value = targets[0].value, let target = URL(string: value),
              target.scheme == "https", target.host == host, target.port == loginURL.port,
              target.user == nil, target.password == nil, target.path == "/ap/maplanding" else { return nil }
        url = target
        amazonDomain = String(host.dropFirst(4))
    }

    func matches(_ candidate: URL) -> Bool {
        guard candidate.absoluteString.utf8.count <= 32768,
              candidate.scheme == url.scheme, candidate.host == url.host, candidate.port == url.port,
              candidate.path == url.path, candidate.user == nil, candidate.password == nil,
              let parts = URLComponents(url: candidate, resolvingAgainstBaseURL: false) else { return false }
        let codes = (parts.queryItems ?? []).filter { $0.name == "openid.oa2.authorization_code" }
        return codes.count == 1 && !(codes[0].value ?? "").isEmpty
    }

    func allowsNavigation(_ candidate: URL) -> Bool {
        if candidate.absoluteString == "about:blank" { return true }
        guard candidate.scheme == "https", candidate.user == nil, candidate.password == nil,
              candidate.port == nil || candidate.port == 443, let host = candidate.host else { return false }
        return host == amazonDomain || host.hasSuffix("." + amazonDomain)
    }
}

func emit(_ kind: String, value: String? = nil) {
    var event = ["kind": kind]
    if let value { event["value"] = value }
    guard let data = try? JSONSerialization.data(withJSONObject: event) else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([10]))
}

final class LoginController: NSObject, NSApplicationDelegate, NSWindowDelegate, WKNavigationDelegate, WKUIDelegate {
    let target: RedirectTarget
    let loginURL: URL
    let configuration: WKWebViewConfiguration
    var window: NSWindow!
    var webView: WKWebView!
    var status: NSTextField!
    var completed = false
    let smokeTest: Bool
    let autofillSmokeTest: Bool
    var passwordSession = PasswordSession()
    var passwordReference: PasswordReference?
    var verifiedPasswordReference: PasswordReference?
    var verifiedPasswordUsername: String?
    var passwordPicker: PasswordItemPicker?
    var passwordBusy = false
    var passwordButton: NSButton!
    var testStep = 0
    var passwordKey: String { "onepassword." + target.amazonDomain }
    var passwordOrigin: String { "https://" + loginURL.host! }

    init(loginURL: URL, target: RedirectTarget, smokeTest: Bool, autofillSmokeTest: Bool = false) {
        self.loginURL = loginURL
        self.target = target
        self.smokeTest = smokeTest
        self.autofillSmokeTest = autofillSmokeTest
        configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        super.init()
        if !smokeTest && !autofillSmokeTest,
           let saved = UserDefaults.standard.dictionary(forKey: passwordKey) as? [String: String] {
            passwordReference = PasswordReference(saved)
        }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let size = NSSize(width: 880, height: 760)
        window = NSWindow(contentRect: NSRect(origin: .zero, size: size),
                          styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Audible sign-in — \(target.amazonDomain)"
        window.minSize = NSSize(width: 720, height: 560)
        window.isReleasedWhenClosed = false
        window.delegate = self
        let content = NSView(frame: NSRect(origin: .zero, size: size))
        window.contentView = content

        let heading = NSTextField(labelWithString: "Sign in with Amazon. The app will capture the result automatically.")
        heading.frame = NSRect(x: 16, y: size.height - 36, width: size.width - 32, height: 22)
        heading.autoresizingMask = [.width, .minYMargin]
        content.addSubview(heading)

        let privacy = NSTextField(labelWithString: "Private sign-in window · login cookies are discarded when it closes.")
        privacy.frame = NSRect(x: 16, y: size.height - 60, width: size.width - 32, height: 18)
        privacy.autoresizingMask = [.width, .minYMargin]
        privacy.font = .systemFont(ofSize: 12)
        privacy.textColor = .secondaryLabelColor
        content.addSubview(privacy)

        webView = WKWebView(frame: NSRect(x: 0, y: 50, width: size.width, height: size.height - 120), configuration: configuration)
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        webView.uiDelegate = self
        content.addSubview(webView)

        passwordButton = NSButton(title: "Fill with 1Password", target: self, action: #selector(fillWithPassword))
        passwordButton.frame = NSRect(x: 16, y: 10, width: 172, height: 30)
        content.addSubview(passwordButton)
        let passwordOptions = NSButton(title: "⋯", target: self, action: #selector(showPasswordOptions(_:)))
        passwordOptions.frame = NSRect(x: 192, y: 10, width: 32, height: 30)
        passwordOptions.setAccessibilityLabel("1Password options")
        content.addSubview(passwordOptions)
        let retry = NSButton(title: "Retry sign-in", target: self, action: #selector(reloadSignIn))
        retry.frame = NSRect(x: 236, y: 10, width: 120, height: 30)
        content.addSubview(retry)
        let cancelButton = NSButton(title: "Cancel", target: self, action: #selector(cancel))
        cancelButton.frame = NSRect(x: 368, y: 10, width: 80, height: 30)
        content.addSubview(cancelButton)
        status = NSTextField(wrappingLabelWithString: "Loading Amazon sign-in…")
        status.frame = NSRect(x: 464, y: 8, width: size.width - 480, height: 34)
        status.autoresizingMask = [.width]
        status.font = .systemFont(ofSize: 12)
        status.textColor = .secondaryLabelColor
        content.addSubview(status)

        let menu = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Cancel sign-in", action: #selector(cancel), keyEquivalent: "q").target = self
        appMenu.addItem(withTitle: "Close sign-in window", action: #selector(cancel), keyEquivalent: "w").target = self
        appMenu.addItem(withTitle: "Retry sign-in", action: #selector(reloadSignIn), keyEquivalent: "r").target = self
        appMenu.addItem(withTitle: "Fill with 1Password", action: #selector(fillWithPassword), keyEquivalent: "p").target = self
        appMenu.addItem(withTitle: "Choose a different 1Password login", action: #selector(changePasswordLogin), keyEquivalent: "").target = self
        appItem.submenu = appMenu
        menu.addItem(appItem)
        let editItem = NSMenuItem()
        editItem.title = "Edit"
        let editMenu = NSMenu(title: "Edit")
        for (title, selector, key) in [("Cut", "cut:", "x"), ("Copy", "copy:", "c"), ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a")] {
            editMenu.addItem(withTitle: title, action: Selector(selector), keyEquivalent: key)
        }
        editItem.submenu = editMenu
        menu.addItem(editItem)
        NSApplication.shared.mainMenu = menu

        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
        window.makeFirstResponder(webView)
        emit("webview_opened")

        if autofillSmokeTest {
            loadPasswordTestPage()
            DispatchQueue.main.asyncAfter(deadline: .now() + 12) { [weak self] in self?.finish("webview_error") }
        } else if smokeTest {
            let redirect = target.url.absoluteString + "?openid.oa2.authorization_code=synthetic-test-code"
            webView.loadHTMLString("<html><body>Offline redirect check<script>setTimeout(() => location.href = '\(redirect)', 100)</script></body></html>", baseURL: loginURL)
            DispatchQueue.main.asyncAfter(deadline: .now() + 12) { [weak self] in
                self?.finish("webview_error")
            }
        } else {
            webView.load(URLRequest(url: loginURL))
        }

        DispatchQueue.global().async { [weak self] in
            while let line = readLine() {
                guard let data = line.data(using: .utf8),
                      let message = try? JSONSerialization.jsonObject(with: data) as? [String: String] else { continue }
                if message["kind"] == "cancel" {
                    DispatchQueue.main.async { self?.finish("webview_closed") }
                    return
                }
                if message["kind"] == "show" {
                    DispatchQueue.main.async {
                        self?.window.deminiaturize(nil)
                        self?.window.makeKeyAndOrderFront(nil)
                        NSApplication.shared.activate(ignoringOtherApps: true)
                    }
                }
            }
            DispatchQueue.main.async { self?.finish("webview_closed") }
        }
    }

    func finish(_ kind: String, value: String? = nil) {
        guard !completed else { return }
        completed = true
        passwordSession.cancel()
        if let sheet = window?.attachedSheet { window.endSheet(sheet) }
        emit(kind, value: value)
        webView?.stopLoading()
        webView?.navigationDelegate = nil
        webView?.uiDelegate = nil
        window?.orderOut(nil)
        NSApplication.shared.terminate(nil)
    }

    @objc func reloadSignIn() {
        guard !completed && !passwordBusy else { return }
        verifiedPasswordReference = nil
        verifiedPasswordUsername = nil
        status.stringValue = "Loading Amazon sign-in…"
        webView.load(URLRequest(url: loginURL))
    }

    @objc func cancel() { finish("webview_closed") }
    func windowWillClose(_ notification: Notification) { cancel() }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard !completed, let url = action.request.url else {
            decisionHandler(.cancel)
            return
        }
        let isMainFrame = action.targetFrame?.isMainFrame ?? true
        if isMainFrame && url.path == "/ap/signin" && [.linkActivated, .backForward].contains(action.navigationType) {
            verifiedPasswordReference = nil
            verifiedPasswordUsername = nil
        }
        if isMainFrame && target.matches(url) {
            decisionHandler(.cancel)
            finish("webview_redirect", value: url.absoluteString)
        } else if isMainFrame && !target.allowsNavigation(url) {
            decisionHandler(.cancel)
            status.stringValue = "Sign-in navigation was blocked. Retry or cancel."
            emit("webview_navigation_blocked")
        } else {
            decisionHandler(.allow)
        }
    }

    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if action.targetFrame == nil, let url = action.request.url, target.allowsNavigation(url) {
            webView.load(action.request)
        }
        return nil
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        reportFailure(error)
    }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) { reportFailure(error) }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if !completed {
            if !passwordBusy { status.stringValue = "Complete sign-in here. The result is captured automatically." }
            emit("webview_ready")
            if autofillSmokeTest { runPasswordPageTest() }
            else if passwordReference != nil { fillPasswordPage(automatic: true) }
        }
    }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        if !completed {
            status.stringValue = "The sign-in page stopped. Retry or cancel."
            emit("webview_load_failed")
        }
    }
    func reportFailure(_ error: Error) {
        if !completed && (error as NSError).code != NSURLErrorCancelled {
            status.stringValue = "Amazon sign-in could not load. Retry or cancel."
            emit("webview_load_failed")
        }
    }
}

func selfTest() {
    let login = URL(string: "https://www.amazon.co.uk/ap/signin?openid.return_to=https%3A%2F%2Fwww.amazon.co.uk%2Fap%2Fmaplanding")!
    let target = RedirectTarget(loginURL: login)!
    let good = "https://www.amazon.co.uk/ap/maplanding?openid.oa2.authorization_code=synthetic-test-code"
    assert(target.matches(URL(string: good)!))
    for bad in [good.replacingOccurrences(of: "https:", with: "http:"),
                good.replacingOccurrences(of: "amazon.co.uk", with: "amazon.co.uk.evil.example"),
                good.replacingOccurrences(of: "/ap/maplanding", with: "/ap/signin"),
                good + "&openid.oa2.authorization_code=duplicate",
                "https://www.amazon.co.uk/ap/maplanding", good.replacingOccurrences(of: "://", with: "://user@"),
                good.replacingOccurrences(of: "synthetic-test-code", with: "")] {
        assert(!target.matches(URL(string: bad)!))
    }
    assert(target.allowsNavigation(login))
    assert(!target.allowsNavigation(URL(string: "https://www.amazon.co.uk.evil.example/")!))
    assert(!target.allowsNavigation(URL(string: "http://www.amazon.co.uk/")!))
    assert(!target.allowsNavigation(URL(string: "file:///etc/passwd")!))
    assert(RedirectTarget(loginURL: URL(string: "https://evil.example/?openid.return_to=https%3A%2F%2Fevil.example%2Fap%2Fmaplanding")!) == nil)
    passwordSelfTest()
    let configuration = WKWebViewConfiguration()
    configuration.websiteDataStore = .nonPersistent()
    assert(!configuration.websiteDataStore.isPersistent)
    print("WebKit redirect validation and ephemeral storage checks passed.")
}

@main
enum AudibleLoginMain {
static func main() {
if CommandLine.arguments.contains("--check-password-cli") {
    do {
        guard let accounts = try PasswordSession().run(["account", "list", "--format=json"], timeout: 10) as? [[String: Any]],
              accounts.contains(where: { ($0["account_uuid"] as? String)?.range(of: "^[A-Za-z0-9]{26}$", options: .regularExpression) != nil }) else { throw PasswordError.invalid }
        print("1Password account discovery succeeded; no credentials requested.")
    } catch {
        print((error as? PasswordError ?? .failed).message)
        exit(1)
    }
} else if CommandLine.arguments.contains("--self-test") {
    selfTest()
} else if let line = readLine(), let data = line.data(using: .utf8),
          let message = try? JSONSerialization.jsonObject(with: data) as? [String: String],
          let value = message["url"], let loginURL = URL(string: value), let target = RedirectTarget(loginURL: loginURL) {
    let app = NSApplication.shared
    let controller = LoginController(loginURL: loginURL, target: target,
                                     smokeTest: CommandLine.arguments.contains("--smoke-test"),
                                     autofillSmokeTest: CommandLine.arguments.contains("--autofill-smoke-test"))
    app.setActivationPolicy(.accessory)
    app.delegate = controller
    app.run()
    withExtendedLifetime(controller) {}
} else {
    emit("webview_error")
    exit(1)
}
}
}
