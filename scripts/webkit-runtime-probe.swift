import Darwin
import Foundation
import WebKit

final class ProbeHandler: NSObject, WKNavigationDelegate {
    private let requireJspi: Bool
    private let source: String
    private var finished = false

    init(requireJspi: Bool, source: String) {
        self.requireJspi = requireJspi
        self.source = source
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        let program = source.replacingOccurrences(of: "export ", with: "") + """

        const jspi = await supportsJspi(WebAssembly);
        const automatic = await selectClient(WebAssembly);
        const forced = await selectClient(WebAssembly, 'asyncify');
        return { jspi, automatic: automatic.mode, forced: forced.mode };
        """
        Task { @MainActor in
            do {
                let value = try await webView.callAsyncJavaScript(
                    program,
                    arguments: [:],
                    in: nil,
                    contentWorld: .page
                )
                self.validate(value)
            } catch {
                self.finish("page probe failed: \(error.localizedDescription)")
            }
        }
    }

    private func validate(_ value: Any?) {
        guard let result = value as? [String: Any] else {
            finish("probe returned a non-object result")
            return
        }
        guard
            let jspi = result["jspi"] as? Bool,
            let automatic = result["automatic"] as? String,
            let forced = result["forced"] as? String
        else {
            finish("probe result omitted a required field")
            return
        }
        let expected = jspi ? "jspi" : "asyncify"
        guard automatic == expected else {
            finish("automatic selection \(automatic) disagrees with functional probe \(jspi)")
            return
        }
        guard forced == "asyncify" else {
            finish("forced Asyncify selected \(forced)")
            return
        }
        guard !requireJspi || (jspi && automatic == "jspi") else {
            finish("this authorized macOS 27 host did not prove functional JSPI")
            return
        }
        print("webkit runtime probe: jspi=\(jspi) automatic=\(automatic) forced=\(forced)")
        finished = true
        exit(0)
    }

    func webView(
        _ webView: WKWebView,
        didFail navigation: WKNavigation!,
        withError error: Error
    ) {
        finish("navigation failed: \(error.localizedDescription)")
    }

    func webView(
        _ webView: WKWebView,
        didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        finish("provisional navigation failed: \(error.localizedDescription)")
    }

    func timeout() {
        finish("functional JSPI/Asyncify probe timed out")
    }

    private func finish(_ message: String) {
        guard !finished else { return }
        finished = true
        fputs("webkit runtime probe: \(message)\n", stderr)
        exit(1)
    }
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard arguments == [] || arguments == ["--require-jspi"] else {
    fputs("usage: swift scripts/webkit-runtime-probe.swift [--require-jspi]\n", stderr)
    exit(2)
}

let root = URL(fileURLWithPath: FileManager.default.currentDirectoryPath, isDirectory: true)
let runtime = root.appendingPathComponent("web/client-runtime.js")
guard let source = try? String(contentsOf: runtime, encoding: .utf8) else {
    fputs("webkit runtime probe: cannot read web/client-runtime.js from this directory\n", stderr)
    exit(2)
}

let configuration = WKWebViewConfiguration()
configuration.websiteDataStore = .nonPersistent()
let handler = ProbeHandler(
    requireJspi: arguments == ["--require-jspi"],
    source: source
)
let view = WKWebView(frame: .zero, configuration: configuration)
view.navigationDelegate = handler
view.loadHTMLString("<!doctype html><meta charset=utf-8>", baseURL: nil)
DispatchQueue.main.asyncAfter(deadline: .now() + 10) {
    handler.timeout()
}
RunLoop.main.run()
