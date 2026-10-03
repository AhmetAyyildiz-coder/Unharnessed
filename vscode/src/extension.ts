import { homedir } from "node:os";
import * as vscode from "vscode";
import { Agent } from "./agent";
import { Providers } from "./providers";
import { Session } from "./session";
import { Settings } from "./settings";
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

  context.subscriptions.push(
    log,
    vscode.window.registerWebviewViewProvider(
      ChatViewProvider.viewId,
      new ChatViewProvider(context.extensionUri, agent, providers, session, settings, log),
    ),
  );
  agent.start();
}

export async function deactivate(): Promise<void> {
  await agent?.stop();
}
