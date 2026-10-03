import { randomBytes } from "node:crypto";
import * as vscode from "vscode";
import type { Agent } from "./agent";
import type { HostMessage, WebviewMessage } from "./protocol";
import type { Providers } from "./providers";
import type { Session } from "./session";
import type { Settings } from "./settings";

/** The panel in the right side bar: hosts the React app from dist/webview. */
export class ChatViewProvider implements vscode.WebviewViewProvider {
  static readonly viewId = "unharnessed.chat";

  private view: vscode.WebviewView | undefined;
  private splashPlayed = false;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly agent: Agent,
    private readonly providers: Providers,
    private readonly session: Session,
    private readonly settings: Settings,
    private readonly log: vscode.LogOutputChannel,
  ) {
    agent.onChange((state) => this.post({ type: "agent", agent: state }));
    providers.onChange(() =>
      this.post({ type: "providers", providers: providers.providers, login: providers.signIn }),
    );
    session.onChange(() => this.post({ type: "session", session: session.current }));
    settings.onChange(() => this.post({ type: "settings", settings: settings.current }));
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
          session: this.session.current,
          settings: this.settings.current,
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
      case "login": {
        // Try again after a failed MCP sign-in: the same server, by the same name.
        const { providerId } = message;
        if (providerId.startsWith("mcp:")) this.mcpSignIn(providerId.slice(4), this.providers.signIn?.name);
        else void this.providers.startLogin(providerId);
        break;
      }
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
      case "setConfig": {
        const { type: _, ...change } = message;
        void this.session.setOption(change);
        break;
      }
      case "setSetting": {
        const { key } = message;
        void this.settings.set(key, message.value).then((error) => {
          this.post({ type: "settingResult", key, error });
        });
        break;
      }
      case "mcpSignIn":
        this.mcpSignIn(message.id, message.label);
        break;
      case "mcpSignOut": {
        const key = `mcp:${message.id}`;
        void this.settings.signOut(message.id).then((error) => this.post({ type: "settingResult", key, error }));
        break;
      }
      case "setSkill": {
        const key = `skill:${message.path}`;
        void this.settings
          .setSkill(message.path, message.enabled)
          .then((error) => this.post({ type: "settingResult", key, error }));
        break;
      }
      case "createSkill":
        void this.settings.createSkill(message.name, message.description).then(({ created, error }) => {
          this.post({ type: "settingResult", key: "skills/create", error });
          if (created) void this.open(created);
        });
        break;
      case "skillFolder":
        void this.settings
          .skillFolder(message.path, message.add)
          .then((error) => this.post({ type: "settingResult", key: "skills/folder", error }));
        break;
      case "openFile":
        // Only a skill on the list: the panel never names any other file.
        if (this.settings.skillFiles.includes(message.path)) void this.open(message.path);
        break;
      case "copy":
        void vscode.env.clipboard.writeText(message.text);
        break;
    }
  }

  /** An MCP server's sign-in runs on the sign-in screen, like a provider's. */
  private mcpSignIn(id: string, label: string | undefined): void {
    void this.providers.startLogin(`mcp:${id}`, label).then((done) => {
      if (done) this.settings.signedIn(id);
    });
  }

  private async open(path: string): Promise<void> {
    try {
      await vscode.window.showTextDocument(vscode.Uri.file(path), { preview: false });
      this.log.info(`opened ${path}`);
    } catch (error) {
      this.log.warn(`could not open ${path}: ${String(error)}`);
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
