import { randomBytes } from "node:crypto";
import * as vscode from "vscode";
import type { Agent } from "./agent";
import type { HostMessage, WebviewMessage } from "./protocol";
import type { Providers } from "./providers";

/** The panel in the right side bar: hosts the React app from dist/webview. */
export class ChatViewProvider implements vscode.WebviewViewProvider {
  static readonly viewId = "unharnessed.chat";

  private view: vscode.WebviewView | undefined;
  private splashPlayed = false;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly agent: Agent,
    private readonly providers: Providers,
    private readonly log: vscode.LogOutputChannel,
  ) {
    agent.onChange((state) => this.post({ type: "agent", agent: state }));
    providers.onChange(() =>
      this.post({ type: "providers", providers: providers.providers, login: providers.signIn }),
    );
  }

  resolveWebviewView(view: vscode.WebviewView): void {
    const dist = vscode.Uri.joinPath(this.extensionUri, "dist", "webview");
    view.webview.options = { enableScripts: true, localResourceRoots: [dist] };
    view.webview.onDidReceiveMessage((message: WebviewMessage) => this.receive(message));
    view.onDidDispose(() => {
      if (this.view === view) this.view = undefined;
    });
    view.webview.html = renderHtml(view.webview, dist);
    this.view = view;
  }

  private receive(message: WebviewMessage): void {
    switch (message.type) {
      case "ready":
        this.log.info("webview ready");
        this.post({
          type: "init",
          splash: !this.splashPlayed,
          agent: this.agent.current,
          providers: this.providers.providers,
          login: this.providers.signIn,
        });
        this.splashPlayed = true;
        break;
      case "screen":
        this.log.info(`panel: ${message.screen}`);
        break;
      case "restartAgent":
        this.log.info("agent restart requested from the panel");
        void this.agent.restart();
        break;
      case "setApiKey": {
        const { providerId } = message;
        void this.providers.setApiKey(providerId, message.apiKey).then((error) => {
          this.post({ type: "providerResult", action: "apiKey", providerId, error });
        });
        break;
      }
      case "logout": {
        const { providerId } = message;
        void this.providers.logout(providerId).then((error) => {
          this.post({ type: "providerResult", action: "logout", providerId, error });
        });
        break;
      }
      case "login":
        void this.providers.startLogin(message.providerId);
        break;
      case "loginAnswer":
        void this.providers.answer(message.promptId, message.answer);
        break;
      case "loginOpen":
        void this.providers.openPage();
        break;
      case "loginCancel":
        this.providers.cancelLogin();
        break;
      case "loginClose":
        this.providers.closeLogin();
        break;
      case "copy":
        void vscode.env.clipboard.writeText(message.text);
        break;
    }
  }

  private post(message: HostMessage): void {
    void this.view?.webview.postMessage(message);
  }
}

function renderHtml(webview: vscode.Webview, dist: vscode.Uri): string {
  const nonce = randomBytes(16).toString("base64");
  const asset = (name: string) => webview.asWebviewUri(vscode.Uri.joinPath(dist, name)).toString();
  const csp = [
    "default-src 'none'",
    `style-src ${webview.cspSource} 'unsafe-inline'`,
    `font-src ${webview.cspSource}`,
    `img-src ${webview.cspSource} data:`,
    `script-src 'nonce-${nonce}'`,
  ].join("; ");
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="${csp}">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <link rel="preload" href="${asset("OverusedGrotesk-VF.woff2")}" as="font" type="font/woff2" crossorigin>
  <link rel="stylesheet" href="${asset("main.css")}">
</head>
<body>
  <div id="root"></div>
  <script nonce="${nonce}" src="${asset("main.js")}"></script>
</body>
</html>`;
}
