import { randomBytes } from "node:crypto";
import * as vscode from "vscode";

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("Unharnessed", { log: true });
  context.subscriptions.push(
    log,
    vscode.window.registerWebviewViewProvider("unharnessed.chat", new ChatViewProvider(log)),
  );
}

class ChatViewProvider implements vscode.WebviewViewProvider {
  constructor(private readonly log: vscode.LogOutputChannel) {}

  resolveWebviewView(view: vscode.WebviewView): void {
    view.webview.options = { enableScripts: true, localResourceRoots: [] };
    view.webview.onDidReceiveMessage((message: { type?: string }) => {
      if (message.type === "ready") this.log.info("webview ready");
    });
    view.webview.html = renderHtml(view.webview);
  }
}

function renderHtml(webview: vscode.Webview): string {
  const nonce = randomBytes(16).toString("base64");
  const csp = [
    "default-src 'none'",
    `style-src ${webview.cspSource} 'unsafe-inline'`,
    `script-src 'nonce-${nonce}'`,
  ].join("; ");
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="${csp}">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style>
    body {
      color: var(--vscode-foreground);
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      padding: 12px;
    }
    .muted { color: var(--vscode-descriptionForeground); }
  </style>
</head>
<body>
  <h3>Unharnessed</h3>
  <p class="muted">hello</p>
  <script nonce="${nonce}">
    acquireVsCodeApi().postMessage({ type: "ready" });
  </script>
</body>
</html>`;
}
