import { homedir } from "node:os";
import * as vscode from "vscode";
import { Agent } from "./agent";
import { Chat } from "./chat";
import { Files } from "./files";
import { History } from "./history";
import { Kernel } from "./kernel";
import { Providers } from "./providers";
import { Session } from "./session";
import { Settings } from "./settings";
import type { WebviewMessage } from "./protocol";
import { ChatViewProvider } from "./view";

let agent: Agent | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("Unharnessed", { log: true });
  // UNHARNESSED_BIN is set by the F5 launch config and the E2E; otherwise PATH.
  const binary = process.env.UNHARNESSED_BIN || "unharnessed";
  const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? homedir();
  const version = String(context.extension.packageJSON.version);
  agent = new Agent(binary, cwd, version, log);
  // Before `start`: they listen to the agent's notifications.
  const providers = new Providers(agent, log);
  const session = new Session(agent, cwd, log);
  const settings = new Settings(agent, log);
  const files = new Files(cwd, log);
  const chat = new Chat(agent, session, files, log);
  const history = new History(agent, cwd, log);
  const kernel = new Kernel(agent, session, log);
  const view = new ChatViewProvider(
    context.extensionUri,
    agent,
    providers,
    session,
    settings,
    chat,
    history,
    files,
    kernel,
    log,
  );

  context.subscriptions.push(log, vscode.window.registerWebviewViewProvider(ChatViewProvider.viewId, view));
  // The E2E drives the panel's messages from its suite (test/e2e/suite.cjs).
  if (process.env.UNHARNESSED_E2E_LOGS) {
    context.subscriptions.push(
      vscode.commands.registerCommand("unharnessed.e2e.post", (message: WebviewMessage) => view.receive(message)),
    );
  }
  agent.start();
}

export async function deactivate(): Promise<void> {
  await agent?.stop();
}
