import type * as vscode from "vscode";
import { errorText, type Agent, type SavedSession } from "./agent";
import { titleWithoutLinks } from "./files";
import type { HistoryState, SavedChat } from "./protocol";

// Deletes a saved conversation (crates/uh-daemon/src/acp/history.rs).
const DELETE = "_unharnessed/sessions/delete";

/** A `session/list` row, the parts read here. */
type Listed = {
  sessionId: string;
  title?: string;
  updatedAt?: string;
  _meta?: { unharnessed?: { path?: string; current?: boolean } };
};

/**
 * The saved conversations of the panel's folder, for the history menu. The
 * agent lists them from its sessions folder, a file each; the list is read
 * again each time the menu opens, and after a delete.
 */
export class History {
  private chats: SavedChat[] | null = null;
  private files = new Map<string, string>();
  private error: string | undefined;
  private readonly listeners = new Set<() => void>();

  constructor(
    private readonly agent: Agent,
    private readonly cwd: string,
    private readonly log: vscode.LogOutputChannel,
  ) {}

  get current(): HistoryState {
    return { chats: this.chats, error: this.error };
  }

  onChange(listener: () => void): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }

  /** A listed conversation, to start the agent on; `undefined` for the one in the panel. */
  saved(id: string): SavedSession | undefined {
    const path = this.files.get(id);
    const chat = this.chats?.find((chat) => chat.id === id);
    return path && chat && !chat.current ? { id, path } : undefined;
  }

  /** The conversation in the panel, saved once it had a message: the agent restarts on it. */
  async inPanel(): Promise<SavedSession | undefined> {
    await this.list();
    const chat = this.chats?.find((chat) => chat.current);
    const path = chat && this.files.get(chat.id);
    // A stopped agent lists nothing: the conversation it was started on.
    return chat && path ? { id: chat.id, path } : this.agent.resumed;
  }

  async list(): Promise<void> {
    try {
      const { sessions } = await this.agent.request<{ sessions: Listed[] }>("session/list", { cwd: this.cwd });
      this.files = new Map(sessions.map((row) => [row.sessionId, row._meta?.unharnessed?.path ?? ""]));
      this.chats = sessions.map((row) => ({
        id: row.sessionId,
        title: row.title && titleWithoutLinks(row.title),
        updated: row.updatedAt ? Date.parse(row.updatedAt) : 0,
        current: row._meta?.unharnessed?.current === true,
      }));
      this.error = undefined;
      const rows = this.chats.map((chat) => `${chat.id} "${chat.title ?? ""}"${chat.current ? " (current)" : ""}`);
      const listed = `${this.chats.length} conversation${this.chats.length === 1 ? "" : "s"}`;
      this.log.info(`history: ${listed}${rows.length ? `: ${rows.join(", ")}` : ""}`);
    } catch (error) {
      this.error = errorText(error);
      this.log.warn(`history: not listed: ${this.error}`);
    }
    this.changed();
  }

  async remove(id: string): Promise<void> {
    try {
      await this.agent.request(DELETE, { sessionId: id });
      this.log.info(`history: ${id} deleted`);
      await this.list();
    } catch (error) {
      this.error = errorText(error);
      this.log.warn(`history: delete refused: ${this.error}`);
      this.changed();
    }
  }

  private changed(): void {
    for (const listener of this.listeners) listener();
  }
}
