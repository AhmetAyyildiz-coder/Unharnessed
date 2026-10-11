import { homedir } from "node:os";
import * as vscode from "vscode";
import { Conversations } from "./conversations";
import { Files } from "./files";
import { History } from "./history";
import { Providers } from "./providers";
import { Settings } from "./settings";
import { ShownAgent } from "./shown";
import type { WebviewMessage } from "./protocol";
import { ChatViewProvider } from "./view";

let conversations: Conversations | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("Unharnessed", { log: true });
  // UNHARNESSED_BIN is set by the F5 launch config and the E2E; otherwise PATH.
  const binary = process.env.UNHARNESSED_BIN || "unharnessed";
  const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? homedir();
  const version = String(context.extension.packageJSON.version);
  const shown = new ShownAgent(log);
  // Before the first conversation: they listen to every agent's notifications.
  const providers = new Providers(shown, log);
  const settings = new Settings(shown, log);
  const files = new Files(cwd, log);
  // The E2E sleeps a conversation sooner.
  const sleepAfter = process.env.UNHARNESSED_E2E_LOGS ? Number(process.env.UNHARNESSED_E2E_SLEEP_MS) || undefined : undefined;
  conversations = new Conversations(binary, cwd, version, files, shown, log, sleepAfter);
  const history = new History(shown, conversations, cwd, log);
  const view = new ChatViewProvider(context.extensionUri, conversations, providers, settings, history, files, log);

  context.subscriptions.push(log, vscode.window.registerWebviewViewProvider(ChatViewProvider.viewId, view));
  // The E2E drives the panel's messages from its suite (test/e2e/suite.cjs).
  if (process.env.UNHARNESSED_E2E_LOGS) {
    context.subscriptions.push(
      vscode.commands.registerCommand("unharnessed.e2e.post", (message: WebviewMessage) => view.receive(message)),
    );
  }
  conversations.start();
}

/** Every agent stops at once, saving its kernel; one VS Code no longer waits for goes on alone. */
export async function deactivate(): Promise<void> {
  await conversations?.stopAll();
}
