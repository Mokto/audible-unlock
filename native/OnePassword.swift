import AppKit
import Darwin
import Foundation
import WebKit

struct PasswordReference: Equatable {
    let account: String
    let vault: String
    let item: String

    var dictionary: [String: String] { ["account": account, "vault": vault, "item": item] }

    init?(_ value: [String: String]) {
        guard let account = value["account"], let vault = value["vault"], let item = value["item"],
              [account, vault, item].allSatisfy({ $0.range(of: "^[A-Za-z0-9]{26}$", options: .regularExpression) != nil }) else { return nil }
        self.account = account
        self.vault = vault
        self.item = item
    }
}

func isAmazonLogin(_ value: [String: Any]) -> Bool {
    guard value["category"] as? String == "LOGIN", let urls = value["urls"] as? [[String: Any]] else { return false }
    let domains = ["amazon.co.uk", "amazon.com", "amazon.de", "amazon.com.au", "amazon.ca", "amazon.fr",
                   "audible.co.uk", "audible.com", "audible.de", "audible.com.au", "audible.ca", "audible.fr"]
    return urls.contains { entry in
        guard let href = entry["href"] as? String,
              let url = URL(string: href.contains("://") ? href : "https://" + href),
              ["http", "https"].contains(url.scheme), url.user == nil, url.password == nil,
              let host = url.host?.lowercased() else { return false }
        return domains.contains { host == $0 || host.hasSuffix("." + $0) }
    }
}

struct PasswordChoice {
    let reference: PasswordReference
    let title: String
    let detail: String
}

struct PasswordSearchResult {
    let items: [PasswordChoice]
    let failures: [String]
}

func discoverPasswordItems(run: ([String]) throws -> Any) throws -> PasswordSearchResult {
    guard let accounts = try run(["account", "list", "--format=json"]) as? [[String: Any]] else { throw PasswordError.invalid }
    var items: [PasswordChoice] = []
    var failures: [String] = []
    var seen = Set<String>()
    for (index, entry) in accounts.enumerated() {
        guard let account = entry["account_uuid"] as? String,
              account.range(of: "^[A-Za-z0-9]{26}$", options: .regularExpression) != nil, seen.insert(account).inserted else { continue }
        let label = String((entry["email"] as? String ?? entry["url"] as? String ?? "1Password account").prefix(100))
        do {
            guard let entries = try run(["item", "list", "--categories=Login", "--account", account, "--format=json"]) as? [[String: Any]] else { throw PasswordError.invalid }
            for item in entries {
                guard isAmazonLogin(item), let id = item["id"] as? String,
                      let vault = item["vault"] as? [String: Any], let vaultID = vault["id"] as? String,
                      let reference = PasswordReference(["account": account, "vault": vaultID, "item": id]) else { continue }
                let detail = [item["additional_information"] as? String, vault["name"] as? String, label]
                    .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
                items.append(PasswordChoice(reference: reference, title: item["title"] as? String ?? "Amazon login", detail: detail))
            }
        } catch {
            let failure = error as? PasswordError ?? .failed
            if failure == .cancelled { throw failure }
            failures.append(label + " — " + String(failure.message.prefix(100)))
            if failure == .timedOut {
                if index + 1 < accounts.count { failures.append("Remaining accounts were not searched after the timeout.") }
                break
            }
        }
    }
    items.sort { ($0.title, $0.detail) < ($1.title, $1.detail) }
    return PasswordSearchResult(items: items, failures: failures)
}

final class PasswordItemPicker: NSObject {
    let alert = NSAlert()
    let items: [PasswordChoice]
    let rows: [NSButton]

    init(items: [PasswordChoice], message: String) {
        self.items = items
        rows = items.map { _ in NSButton() }
        super.init()
        alert.messageText = "Choose your Amazon login"
        alert.informativeText = message
        alert.addButton(withTitle: "Cancel")
        let height = min(CGFloat(items.count) * 84, 336)
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 520, height: height))
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.drawsBackground = false
        let document = NSView(frame: NSRect(x: 0, y: 0, width: 500, height: CGFloat(items.count) * 84))
        for (index, row) in rows.enumerated() {
            let item = items[index]
            row.frame = NSRect(x: 0, y: document.frame.height - CGFloat(index + 1) * 84 + 6, width: 500, height: 72)
            row.bezelStyle = .regularSquare
            row.cell?.alignment = .left
            row.cell?.wraps = true
            let paragraph = NSMutableParagraphStyle()
            paragraph.alignment = .left
            paragraph.lineBreakMode = .byTruncatingTail
            let title = NSMutableAttributedString(string: String(item.title.prefix(80)) + "\n",
                attributes: [.font: NSFont.boldSystemFont(ofSize: 15), .foregroundColor: NSColor.labelColor, .paragraphStyle: paragraph])
            title.append(NSAttributedString(string: String(item.detail.prefix(140)),
                attributes: [.font: NSFont.systemFont(ofSize: 12), .foregroundColor: NSColor.secondaryLabelColor, .paragraphStyle: paragraph]))
            row.attributedTitle = title
            row.toolTip = item.title + "\n" + item.detail
            row.setAccessibilityLabel(item.title + ", " + item.detail)
            row.tag = index
            row.target = self
            row.action = #selector(selectItem(_:))
            document.addSubview(row)
        }
        scroll.documentView = document
        scroll.contentView.scroll(to: NSPoint(x: 0, y: max(0, document.frame.height - height)))
        alert.accessoryView = scroll
    }

    @objc func selectItem(_ sender: NSButton) {
        guard items.indices.contains(sender.tag) else { return }
        alert.window.sheetParent?.endSheet(alert.window, returnCode: NSApplication.ModalResponse(rawValue: 10000 + sender.tag))
    }

    func reference(for response: NSApplication.ModalResponse) -> PasswordReference? {
        let index = response.rawValue - 10000
        return items.indices.contains(index) ? items[index].reference : nil
    }

    func show(on window: NSWindow, completion: @escaping (PasswordReference?) -> Void) {
        alert.beginSheetModal(for: window) { [weak self] response in completion(self?.reference(for: response)) }
    }
}

struct AmazonPassword {
    let username: String
    let password: String
    let hasOTP: Bool

    init?(_ value: [String: Any], reference: PasswordReference) {
        guard isAmazonLogin(value), value["id"] as? String == reference.item,
              (value["vault"] as? [String: Any])?["id"] as? String == reference.vault,
              let fields = value["fields"] as? [[String: Any]] else { return nil }
        let usernames = fields.filter { $0["id"] as? String == "username" || $0["purpose"] as? String == "USERNAME" }
        let passwords = fields.filter { $0["id"] as? String == "password" || $0["purpose"] as? String == "PASSWORD" }
        guard usernames.count == 1, passwords.count == 1,
              let username = usernames[0]["value"] as? String,
              let password = passwords[0]["value"] as? String,
              !username.isEmpty, !password.isEmpty, username.utf8.count <= 2048, password.utf8.count <= 32768 else { return nil }
        self.username = username
        self.password = password
        hasOTP = fields.contains { $0["type"] as? String == "OTP" && !($0["value"] as? String ?? "").isEmpty }
    }
}

enum PasswordError: Error, Equatable {
    case unavailable, cancelled, timedOut, failed, invalid, connection, denied, exitStatus(Int32)

    static func cli(status: Int32, stderr: Data) -> PasswordError {
        let text = String(data: stderr, encoding: .utf8)?.lowercased() ?? ""
        if text.contains("cancelled") || text.contains("canceled") || text.contains("denied") { return .denied }
        if text.contains("timed out") { return .timedOut }
        if text.contains("connecting to desktop app") || text.contains("connect to 1password") || text.contains("connection reset") { return .connection }
        return .exitStatus(status)
    }
    var message: String {
        switch self {
        case .unavailable: return "Install 1Password CLI (op) with Homebrew, then enable Integrate with 1Password CLI in 1Password → Settings → Developer."
        case .cancelled: return "1Password access was cancelled."
        case .timedOut: return "The 1Password request timed out. Dismiss any pending approval prompt, then try again."
        case .failed: return "The app could not run 1Password CLI. Check that the CLI is installed and 1Password is running, then try again."
        case .connection: return "1Password CLI could not connect to the desktop app. Keep 1Password running and unlocked. If CLI integration is already enabled, restart 1Password and try again."
        case .denied: return "1Password access was denied or cancelled. Try again and approve the access prompt."
        case .exitStatus(let status): return "1Password CLI exited with status \(status). Keep the desktop app running, check CLI integration, then retry. Raw CLI output is not displayed because it can contain private data."
        case .invalid: return "1Password did not return a valid Amazon login. Check the saved item's website, username, and password, or choose a different login."
        }
    }
}

final class PasswordSession {
    private let lock = NSLock()
    private var process: Process?
    private var cancelled = false
    private var timedOut = false
    private let executable: URL?

    init(executable: URL? = nil) {
        self.executable = executable ?? ["/opt/homebrew/bin/op", "/usr/local/bin/op"]
            .first(where: { FileManager.default.isExecutableFile(atPath: $0) }).map { URL(fileURLWithPath: $0) }
    }

    func cancel(ifRunning expected: Process? = nil) {
        lock.lock()
        if let expected, (process !== expected || !expected.isRunning) { lock.unlock(); return }
        cancelled = true
        timedOut = expected != nil
        if let process, process.isRunning {
            process.terminate()
            DispatchQueue.global().asyncAfter(deadline: .now() + 1) {
                if process.isRunning { kill(process.processIdentifier, SIGKILL) }
            }
        }
        lock.unlock()
    }

    func run(_ arguments: [String], timeout: TimeInterval = 120) throws -> Any {
        guard let executable else { throw PasswordError.unavailable }
        let child = Process()
        let output = Pipe()
        let errorOutput = Pipe()
        child.executableURL = executable
        child.arguments = arguments
        child.standardInput = FileHandle.nullDevice
        child.standardOutput = output
        child.standardError = errorOutput
        var environment = ProcessInfo.processInfo.environment.filter { !$0.key.hasPrefix("OP_") }
        environment["OP_BIOMETRIC_UNLOCK_ENABLED"] = "true"
        environment["OP_CACHE"] = "false"
        child.environment = environment
        lock.lock()
        guard !cancelled else { lock.unlock(); throw PasswordError.cancelled }
        do { try child.run() } catch { lock.unlock(); throw PasswordError.failed }
        process = child
        lock.unlock()
        let errors = NSMutableData()
        let errorReader = DispatchGroup()
        errorReader.enter()
        DispatchQueue.global().async {
            defer { errorReader.leave() }
            while let chunk = try? errorOutput.fileHandleForReading.read(upToCount: 4096), !chunk.isEmpty {
                if errors.length < 65536 { errors.append(chunk.prefix(65536 - errors.length)) }
            }
        }
        let deadline = DispatchWorkItem { [weak self, weak child] in
            if let child { self?.cancel(ifRunning: child) }
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: deadline)
        defer {
            deadline.cancel()
            lock.lock()
            process = nil
            lock.unlock()
            output.fileHandleForReading.closeFile()
            errorOutput.fileHandleForReading.closeFile()
        }
        var bytes = Data()
        do {
            while let chunk = try output.fileHandleForReading.read(upToCount: 65536), !chunk.isEmpty {
                guard bytes.count + chunk.count <= 16 * 1024 * 1024 else { cancel(); throw PasswordError.invalid }
                bytes.append(chunk)
            }
        } catch { cancel(); throw PasswordError.failed }
        child.waitUntilExit()
        errorReader.wait()
        lock.lock()
        let wasCancelled = cancelled
        let didTimeOut = timedOut
        lock.unlock()
        guard !wasCancelled else { throw didTimeOut ? PasswordError.timedOut : PasswordError.cancelled }
        guard child.terminationStatus == 0 else { throw PasswordError.cli(status: child.terminationStatus, stderr: Data(referencing: errors)) }
        if arguments.contains("--otp") {
            guard let code = String(data: bytes, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  code.range(of: "^(?:[0-9]{6}|[0-9]{8})$", options: .regularExpression) != nil else { throw PasswordError.invalid }
            return code
        }
        guard let value = try? JSONSerialization.jsonObject(with: bytes) else { throw PasswordError.invalid }
        return value
    }
}

// ponytail: Infer TOTP from visible Amazon prompts; use explicit MFA-device metadata if that contract becomes available.
let passwordFieldsScript = """
function visible(input) { return !input.disabled && !input.readOnly && input.getClientRects().length > 0 && getComputedStyle(input).visibility !== 'hidden'; }
function loginFields() {
    const users = [...document.querySelectorAll('input#ap_email, input[name="email"], input[autocomplete="username"]')].filter(visible);
    const passwords = [...document.querySelectorAll('input#ap_password[type="password"], input[name="password"][type="password"], input[type="password"][autocomplete="current-password"]')].filter(visible);
    const codes = [...document.querySelectorAll('input#auth-mfa-otpcode, input[name="otpCode"], input[autocomplete="one-time-code"]')].filter(visible);
    const text = document.body.innerText.replace(/\\s+/g, ' ');
    const authenticator = /authenticator|authentication app|authentifizierungs|authentifikator|application d.authentification/i.test(text);
    const sentCode = /code.{0,100}(?:sent|texted|gesendet|envoy[eé]|sms)|(?:sent|texted|gesendet|envoy[eé]).{0,100}code/i.test(text);
    if (users.length > 1 || passwords.length > 1 || codes.length > 1) return {kind: 'none', inputs: []};
    if (codes.length && !users.length && !passwords.length) return {kind: authenticator && !sentCode ? 'otp' : 'none', inputs: authenticator && !sentCode ? codes : []};
    return {kind: passwords.length ? 'password' : 'username', inputs: [...users, ...passwords], user: users[0], pass: passwords[0]};
}
"""

let passwordProbeScript = passwordFieldsScript + """
if (location.origin !== origin || !location.pathname.startsWith('/ap/')) return {needed: false};
const fields = loginFields();
globalThis.audiblePasswordDocument ??= Array.from(crypto.getRandomValues(new Uint32Array(4))).join('-');
return {needed: fields.inputs.some(input => !input.value), kind: fields.kind, document: globalThis.audiblePasswordDocument};
"""

let passwordFillScript = passwordFieldsScript + """
if (location.origin !== origin || location.href !== href || !location.pathname.startsWith('/ap/') || globalThis.audiblePasswordDocument !== documentID) return 'stale';
const fields = loginFields(), user = fields.user, pass = fields.pass;
function normalize(value) { value = value.trim().toLowerCase(); return /^\\+?[\\d\\s()-]+$/.test(value) ? value.replace(/[\\s()-]/g, '') : value; }
const claim = user?.value || document.querySelector('input#ap_email')?.value || document.querySelector('input[name="email"]')?.value || document.getElementById('auth-email-claim')?.textContent;
if (claim && normalize(claim) !== normalize(username)) return 'account_mismatch';
if (pass && !user && !claim) return 'account_unknown';
const values = fields.kind === 'otp' ? [[fields.inputs[0], otp]] : [[user, username], [pass, password]];
if (fields.kind === 'otp' && (!/^(?:[0-9]{6}|[0-9]{8})$/.test(otp) || (fields.inputs[0].maxLength > 0 && otp.length > fields.inputs[0].maxLength))) return 'unsupported';
let filled = 0;
for (const [input, value] of values) {
    if (!input || input.value || !value) continue;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, value);
    input.dispatchEvent(new Event('input', {bubbles: true}));
    input.dispatchEvent(new Event('change', {bubbles: true}));
    filled++;
}
return filled ? 'filled' : 'unsupported';
"""

func passwordSelfTest() {
    let id = String(repeating: "a", count: 26)
    let reference = PasswordReference(["account": id, "vault": id, "item": id])!
    assert(PasswordReference(["account": "--evil", "vault": id, "item": id]) == nil)
    var item: [String: Any] = ["id": id, "vault": ["id": id], "category": "LOGIN", "urls": [["href": "https://www.amazon.co.uk"]],
                              "fields": [["id": "username", "value": "fake@example.invalid"], ["id": "password", "value": "quote'\"\\<script>🦄"]]]
    assert(AmazonPassword(item, reference: reference)?.hasOTP == false)
    var withOTP = item
    withOTP["fields"] = (item["fields"] as! [[String: String]]) + [["id": "otp", "type": "OTP", "value": "synthetic-seed"]]
    assert(AmazonPassword(withOTP, reference: reference)?.hasOTP == true)
    for url in ["https://amazon.co.uk.evil.example", "https://evilamazon.com", "file:///etc/passwd", "https://user@amazon.com"] {
        item["urls"] = [["href": url]]
        assert(AmazonPassword(item, reference: reference) == nil)
    }
    item["urls"] = [["href": "https://amazon.com"]]
    item["category"] = "PASSWORD"
    assert(AmazonPassword(item, reference: reference) == nil)
    item["category"] = "LOGIN"
    item["fields"] = [["id": "username", "value": "fake"], ["id": "password", "value": ""]]
    assert(AmazonPassword(item, reference: reference) == nil)
    let session = PasswordSession(executable: URL(fileURLWithPath: "/bin/sh"))
    let result = try! session.run(["-c", "test ! -t 0 && test \"$OP_BIOMETRIC_UNLOCK_ENABLED\" = true && test -z \"$OP_SERVICE_ACCOUNT_TOKEN\" && printf '[]'"])
    assert(result is [Any])
    _ = try! session.run(["-c", "test ! -t 0 && printf '[]'"], timeout: 2)
    assert(PasswordError.cli(status: 1, stderr: Data("connecting to desktop app: connection reset".utf8)) == .connection)
    let privateError = PasswordError.cli(status: 23, stderr: Data("synthetic-private-value".utf8))
    assert(privateError == .exitStatus(23) && !privateError.message.contains("synthetic-private-value"))
    do { _ = try session.run(["-c", "printf 'synthetic-private-value' >&2; exit 23"]); assertionFailure("Failed CLI accepted") }
    catch { assert(error as? PasswordError == .exitStatus(23)) }
    session.cancel()
    do { _ = try session.run(["-c", "printf '[]'"]); assertionFailure("Cancelled session started another process") } catch {}
    let timeout = PasswordSession(executable: URL(fileURLWithPath: "/bin/sh"))
    let started = Date()
    do { _ = try timeout.run(["-c", "exec sleep 20"], timeout: 0.1); assertionFailure("Timeout did not cancel") } catch {}
    assert(Date().timeIntervalSince(started) < 4)
    let codes = PasswordSession(executable: URL(fileURLWithPath: "/bin/sh"))
    assert(try! codes.run(["-c", "printf 012345", "--otp"]) as? String == "012345")
    do { _ = try codes.run(["-c", "printf malformed", "--otp"]); assertionFailure("Malformed OTP accepted") }
    catch { assert(error as? PasswordError == .invalid) }
    let second = String(repeating: "b", count: 26)
    let denied = String(repeating: "c", count: 26)
    var queried: [String] = []
    var login = item
    login["urls"] = [["href": "https://amazon.co.uk"]]
    login["title"] = "Amazon"
    login["vault"] = ["id": id, "name": "Personal"]
    let all = try! discoverPasswordItems { args in
        if args.starts(with: ["account", "list"]) {
            return [["account_uuid": id, "email": "first@example.invalid"], ["account_uuid": denied, "email": "denied@example.invalid"],
                    ["account_uuid": second, "email": "second@example.invalid"], ["account_uuid": id]]
        }
        let account = args[args.firstIndex(of: "--account")! + 1]
        queried.append(account)
        if account == denied { throw PasswordError.denied }
        return [login, ["category": "LOGIN", "urls": [["href": "https://evil.invalid"]]]]
    }
    assert(queried == [id, denied, second] && all.items.count == 2 && all.failures.count == 1)
    assert(Set(all.items.map { $0.reference.account }) == Set([id, second]))
    let picker = PasswordItemPicker(items: all.items, message: "Fake items from all accounts")
    assert(picker.rows.count == 2 && picker.rows.allSatisfy { $0.frame.height >= 70 })
    assert(picker.reference(for: .alertFirstButtonReturn) == nil)
    assert(picker.reference(for: NSApplication.ModalResponse(rawValue: 10001)) == all.items[1].reference)
}

extension LoginController {
    func passwordFailure(_ error: PasswordError, operation: String = "autofill") {
        guard !completed else { return }
        status.stringValue = "1Password could not fill. Try again or choose another login."
        let alert = NSAlert()
        alert.messageText = "1Password — \(operation)"
        alert.informativeText = error.message
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "1Password CLI settings")
        alert.beginSheetModal(for: window) { [weak self] response in
            if self?.completed == false && response == .alertSecondButtonReturn {
                self?.openPasswordSettings()
            }
        }
    }

    @objc func openPasswordSettings() {
        NSWorkspace.shared.open(URL(string: "onepassword://settings/developers")!)
    }

    @objc func showPasswordOptions(_ sender: NSButton) {
        guard !passwordBusy && window.attachedSheet == nil else { return }
        let menu = NSMenu()
        menu.addItem(withTitle: "Choose a different login", action: #selector(changePasswordLogin), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Forget saved choice", action: #selector(forgetPasswordLogin), keyEquivalent: "").target = self
        menu.addItem(withTitle: "1Password CLI settings", action: #selector(openPasswordSettings), keyEquivalent: "").target = self
        menu.popUp(positioning: nil, at: NSPoint(x: sender.frame.minX, y: sender.frame.maxY), in: window.contentView)
    }

    @objc func forgetPasswordLogin() {
        guard !passwordBusy && !completed else { return }
        passwordReference = nil
        verifiedPasswordReference = nil
        verifiedPasswordUsername = nil
        UserDefaults.standard.removeObject(forKey: passwordKey)
        status.stringValue = "Saved 1Password choice forgotten."
    }

    @objc func changePasswordLogin() {
        forgetPasswordLogin()
        fillWithPassword()
    }

    @objc func fillWithPassword() { fillPasswordPage(automatic: false) }

    func passwordCommand(_ arguments: [String], completion: @escaping (Any) -> Void) {
        let operation = arguments.contains("--otp") ? "OTP generation"
            : arguments.starts(with: ["account", "list"]) ? "account discovery"
            : arguments.starts(with: ["item", "list"]) ? "login selection" : "credential retrieval"
        passwordTask(operation: operation, work: { try $0.run(arguments) }, completion: completion)
    }

    func passwordTask(operation: String, work: @escaping (PasswordSession) throws -> Any, completion: @escaping (Any) -> Void) {
        guard !completed && !passwordBusy else { return }
        passwordBusy = true
        passwordButton.isEnabled = false
        status.stringValue = "Approve access in 1Password…"
        let session = passwordSession
        DispatchQueue.global().async { [weak self] in
            let result = Result { try work(session) }
            DispatchQueue.main.async {
                guard let self, !self.completed else { return }
                self.passwordBusy = false
                self.passwordButton.isEnabled = true
                switch result {
                case .success(let value): completion(value)
                case .failure(let error):
                    session.cancel()
                    self.passwordSession = PasswordSession()
                    self.passwordFailure(error as? PasswordError ?? .failed, operation: operation)
                }
            }
        }
    }

    func choosePasswordItem(completion: @escaping (PasswordReference) -> Void) {
        passwordTask(operation: "login search", work: { session in
            try discoverPasswordItems(run: { try session.run($0) })
        }) { [weak self] value in
            guard let self, let result = value as? PasswordSearchResult else { return }
            self.passwordSession = PasswordSession()
            let warning = result.failures.isEmpty ? "" : "\n\nNot all accounts could be searched:\n" + result.failures.prefix(3).joined(separator: "\n")
                + (result.failures.count > 3 ? "\nAnd \(result.failures.count - 3) more unavailable accounts." : "")
            guard !result.items.isEmpty else {
                let alert = NSAlert()
                alert.messageText = "No matching 1Password login"
                alert.informativeText = "No Amazon/Audible Login items were found across your accounts and vaults. Add an Amazon or Audible website URL to your Login item, then try again." + warning
                alert.beginSheetModal(for: self.window)
                return
            }
            let picker = PasswordItemPicker(items: result.items,
                message: "Choose a login from any account or vault. This choice is remembered for \(self.target.amazonDomain); passwords and authenticator codes are never saved." + warning)
            self.passwordPicker = picker
            picker.show(on: self.window) { [weak self] reference in
                guard let self, !self.completed else { return }
                self.passwordPicker = nil
                if let reference { completion(reference) }
                else { self.status.stringValue = "1Password selection cancelled." }
            }
        }
    }

    func fillPasswordPage(automatic: Bool) {
        guard !completed && !passwordBusy && window.attachedSheet == nil,
              let url = webView.url, url.scheme == "https", url.host == loginURL.host,
              url.port == nil || url.port == 443, url.path.hasPrefix("/ap/") else { return }
        let href = url.absoluteString
        passwordBusy = true
        passwordButton.isEnabled = false
        webView.callAsyncJavaScript(passwordProbeScript, arguments: ["origin": passwordOrigin], in: nil, in: .defaultClient) { [weak self] result in
            guard let self, !self.completed else { return }
            self.passwordBusy = false
            self.passwordButton.isEnabled = true
            guard case .success(let raw) = result, let probe = raw as? [String: Any], probe["needed"] as? Bool == true,
                  let documentID = probe["document"] as? String else {
                if !automatic { self.status.stringValue = "No empty login fields. Enter SMS/email codes manually, or clear a field." }
                return
            }
            let selected: (PasswordReference) -> Void = { reference in
                guard self.webView.url?.absoluteString == href, !self.completed else { return }
                self.passwordCommand(["item", "get", reference.item, "--vault", reference.vault, "--account", reference.account, "--format=json", "--reveal"]) { value in
                    guard let item = value as? [String: Any], let credentials = AmazonPassword(item, reference: reference) else {
                        self.passwordFailure(.invalid)
                        return
                    }
                    let isOTP = probe["kind"] as? String == "otp"
                    let inject: (String) -> Void = { otp in
                        self.injectPassword(credentials, href: href, documentID: documentID, otp: otp) { filled in
                            if filled {
                                self.passwordReference = reference
                                if !isOTP {
                                    self.verifiedPasswordReference = reference
                                    self.verifiedPasswordUsername = credentials.username
                                }
                                UserDefaults.standard.set(reference.dictionary, forKey: self.passwordKey)
                            }
                        }
                    }
                    if isOTP {
                        guard credentials.hasOTP else {
                            self.status.stringValue = "This login has no authenticator code. Add it in 1Password or enter the code manually."
                            return
                        }
                        guard !automatic || (self.verifiedPasswordReference == reference && self.verifiedPasswordUsername == credentials.username) else {
                            self.status.stringValue = "Click Fill with 1Password to confirm the OTP login."
                            return
                        }
                        self.passwordCommand(["item", "get", reference.item, "--vault", reference.vault, "--account", reference.account, "--otp"]) { value in
                            guard let code = value as? String else { self.passwordFailure(.invalid); return }
                            inject(code)
                        }
                    } else { inject("") }
                }
            }
            if let reference = self.passwordReference { selected(reference) }
            else { self.choosePasswordItem(completion: selected) }
        }
    }

    func injectPassword(_ credentials: AmazonPassword, href: String, documentID: String, otp: String = "", completion: @escaping (Bool) -> Void) {
        guard !completed, webView.url?.absoluteString == href else { completion(false); return }
        passwordBusy = true
        passwordButton.isEnabled = false
        webView.callAsyncJavaScript(passwordFillScript,
            arguments: ["origin": passwordOrigin, "href": href, "documentID": documentID, "username": credentials.username, "password": credentials.password, "otp": otp],
            in: nil, in: .defaultClient) { [weak self] result in
                guard let self, !self.completed else { return }
                self.passwordBusy = false
                self.passwordButton.isEnabled = true
                let code = (try? result.get()) as? String
                switch code {
                case "filled": self.status.stringValue = otp.isEmpty ? "Filled. Click Amazon's Continue or Sign in." : "Authenticator code filled. Click Amazon's Verify."
                case "account_mismatch":
                    self.verifiedPasswordReference = nil
                    self.verifiedPasswordUsername = nil
                    self.status.stringValue = "Amazon shows a different account. Use its Change link first."
                case "account_unknown": self.status.stringValue = "Cannot verify the Amazon account. Enter the password manually."
                case "stale": self.status.stringValue = "The page changed. Click Fill with 1Password again."
                default: self.status.stringValue = "Could not fill this page. Enter the login manually."
                }
                completion(code == "filled")
                if code == "filled" && self.passwordReference != nil { self.fillPasswordPage(automatic: true) }
            }
    }

    func loadPasswordTestPage() {
        let body: String
        switch testStep {
        case 0: body = "<input id='ap_email' autocomplete='username'>"
        case 1: body = "<span id='auth-email-claim'>fake@example.invalid</span><input id='ap_password' type='password' autocomplete='current-password'>"
        case 2: body = "<span id='auth-email-claim'>other@example.invalid</span><input id='ap_password' type='password'>"
        case 4, 5: body = "<span id='auth-email-claim'>fake@example.invalid</span><input id='ap_password' type='password'>"
        case 6: body = "<input id='ap_email' value='already-filled'><input id='ap_password' type='password' value='already-filled'>"
        case 7: body = "<p>Enter the code from your authenticator app</p><span id='auth-email-claim'>fake@example.invalid</span><input id='auth-mfa-otpcode' name='otpCode' autocomplete='one-time-code'>"
        case 8: body = "<p>Enter the code from your authenticator app</p><span id='auth-email-claim'>other@example.invalid</span><input id='auth-mfa-otpcode' name='otpCode'>"
        case 9: body = "<p>Authenticator app</p><input id='auth-mfa-otpcode' value='123456' autocomplete='one-time-code'>"
        case 10: body = "<p>Authenticator app</p><input name='otpCode'><input autocomplete='one-time-code'>"
        case 11: body = "<p>Enter the characters shown in the image</p><input name='captcha'>"
        default: body = "<p>Enter the code we sent by SMS. Or use an authenticator app.</p><input id='otp' autocomplete='one-time-code'>"
        }
        webView.loadHTMLString("<html><body>\(body)<script>document.body.dataset.events='0';document.addEventListener('input',()=>document.body.dataset.events=String(Number(document.body.dataset.events)+1));</script></body></html>", baseURL: loginURL)
    }

    func runPasswordPageTest() {
        let id = String(repeating: "a", count: 26)
        let reference = PasswordReference(["account": id, "vault": id, "item": id])!
        let credentials = AmazonPassword(["id": id, "vault": ["id": id], "category": "LOGIN", "urls": [["href": "https://amazon.co.uk"]],
                                          "fields": [["id": "username", "value": "fake@example.invalid"], ["id": "password", "value": "quote'\"\\<script>🦄"]]], reference: reference)!
        let href = webView.url!.absoluteString
        webView.callAsyncJavaScript(passwordProbeScript, arguments: ["origin": passwordOrigin], in: nil, in: .defaultClient) { [weak self] result in
            guard let self else { return }
            guard case .success(let raw) = result, let probe = raw as? [String: Any] else { self.finish("webview_error"); return }
            if [3, 6, 9, 10, 11].contains(self.testStep) {
                guard probe["needed"] as? Bool == false else { self.finish("webview_error"); return }
                if self.testStep == 11 { self.finish("webview_autofill_checked") }
                else { self.testStep += 1; self.loadPasswordTestPage() }
                return
            }
            guard probe["needed"] as? Bool == true, let documentID = probe["document"] as? String else { self.finish("webview_error"); return }
            if self.testStep == 4 || self.testStep == 5 {
                self.webView.callAsyncJavaScript(passwordFillScript, arguments: [
                    "origin": self.testStep == 4 ? "https://evil.invalid" : self.passwordOrigin,
                    "href": href, "documentID": self.testStep == 5 ? "stale-document" : documentID,
                    "username": credentials.username, "password": credentials.password], in: nil, in: .defaultClient) { result in
                        guard (try? result.get()) as? String == "stale" else { self.finish("webview_error"); return }
                        self.webView.callAsyncJavaScript("return document.getElementById('ap_password').value === '' && document.body.dataset.events === '0';", arguments: [:], in: nil, in: .defaultClient) { result in
                            guard (try? result.get()) as? Bool == true else { self.finish("webview_error"); return }
                            self.testStep += 1
                            self.loadPasswordTestPage()
                        }
                }
                return
            }
            self.injectPassword(credentials, href: href, documentID: documentID, otp: "012345") { filled in
                guard filled == ([0, 1, 7].contains(self.testStep)) else { self.finish("webview_error"); return }
                let check = self.testStep == 0 ? "return document.getElementById('ap_email').value === username && document.body.dataset.events === '1';"
                    : self.testStep == 1 ? "return document.getElementById('ap_password').value === password && document.body.dataset.events === '1';"
                    : self.testStep == 7 ? "return document.getElementById('auth-mfa-otpcode').value === otp && document.body.dataset.events === '1';"
                    : self.testStep == 8 ? "return document.getElementById('auth-mfa-otpcode').value === '' && document.body.dataset.events === '0';"
                    : "return document.getElementById('ap_password').value === '' && document.body.dataset.events === '0';"
                self.webView.callAsyncJavaScript(check, arguments: ["username": credentials.username, "password": credentials.password, "otp": "012345"], in: nil, in: .defaultClient) { result in
                    guard (try? result.get()) as? Bool == true else { self.finish("webview_error"); return }
                    self.testStep += 1
                    self.loadPasswordTestPage()
                }
            }
        }
    }
}
