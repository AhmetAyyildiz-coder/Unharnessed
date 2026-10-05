import type * as vscode from "vscode";
import { errorText, type Agent } from "./agent";
import type {
  Attachment,
  ChatCommand,
  ChatEntry,
  ChatState,
  EntryHead,
  HostMessage,
  QueuedMessage,
  Thumb,
  TurnHead,
  TurnItem,
} from "./protocol";
import type { Session } from "./session";

/** How long streamed text gathers before the panel gets it. */
const FLUSH_MS = 40;
/** A tool's output past this is cut for the panel; the agent has it all. */
const OUTPUT_LIMIT = 20_000;

/** The panel's own commands, beside the agent's. */
const PANEL_COMMANDS: ChatCommand[] = [{ name: "new", description: "Start a new conversation", kind: "panel" }];

const WAKES = { bash: "a background command ended", quota: "a usage limit reset", goal: "a goal's pause ended" };

type Turn = TurnHead & { items: TurnItem[] };
type Queued = { id: number; text: string; images: Attachment[] };
type Tool = Extract<TurnItem, { kind: "tool" }>;
export type ChatMessage = Extract<HostMessage, { type: "chat" | "chatPatch" | "chatDraft" }>;

/** `_meta.unharnessed` on an update (crates/uh-daemon/src/acp/meta.rs), the parts read here. */
type Meta = {
  promptTurnId?: number;
  phase?: "event" | "responseBoundary" | "terminalQuiescence";
  outcome?: "result" | "error";
  turnStart?: { origin: "prompt" | "wake"; wake?: keyof typeof WAKES };
  command?: { name: string; text: string; severity: string };
  thinking?: { state: "start" | "end"; messageId: string };
  compaction?: { tokensBefore?: number; summary?: string };
  goal?: { status: string; objective?: string };
  ipython?: { attachments?: unknown[]; diffCount?: number };
};

/** A `session/update`'s `update`, the parts read here. */
type Update = {
  sessionUpdate: string;
  messageId?: string;
  content?: { text?: string } | { content?: { text?: string } }[];
  toolCallId?: string;
  title?: string;
  status?: string;
  rawInput?: unknown;
  availableCommands?: {
    name: string;
    description: string;
    input?: { hint?: string } | null;
    _meta?: { unharnessed?: { kind?: string } };
  }[];
  _meta?: { unharnessed?: Meta };
};

/**
 * The conversation in the session: what the user sent, what the agent
 * streamed back, the turns it started on its own (a background command
 * ending, a usage limit resetting). One turn runs at a time: a message
 * sent meanwhile waits, then goes when the turn ends. It outlives the
 * panel; the panel gets it whole when it opens, then what changed.
 */
export class Chat {
  private entries: ChatEntry[] = [];
  private queue: Queued[] = [];
  private commands: ChatCommand[] = PANEL_COMMANDS;
  private nextId = 1;
  /** Our prompt, sent and not answered yet. */
  private sent: Turn | undefined;
  /** The turn the agent's updates go to: ours, or one it started. */
  private live: Turn | undefined;
  /** The agent's turn ids, ours for this process. */
  private turns = new Map<number, Turn>();
  /** The last streamed item: chunks of the same message and kind join it. */
  private streaming: { item: TurnItem; key: string } | undefined;
  /** The reasoning under way, timed from when it opened. */
  private thinking: { turn: Turn; at: number } | undefined;
  /** The goal's status, to tell when it changes. */
  private goal: string | undefined;
  /** Bumped per agent process, so a late answer never lands on the next one. */
  private run = 0;
  private dirtyHeads = new Set<number>();
  private dirtyItems = new Map<number, Set<number>>();
  private commandsChanged = false;
  private timer: NodeJS.Timeout | undefined;
  private readonly listeners = new Set<(message: ChatMessage) => void>();

  constructor(
    private readonly agent: Agent,
    private readonly session: Session,
    private readonly log: vscode.LogOutputChannel,
  ) {
    agent.onNotification("session/update", (params) => this.onUpdate(params));
    agent.onChange((state) => {
      this.run++;
      this.turns.clear();
      this.streaming = undefined;
      this.goal = undefined;
      this.commands = PANEL_COMMANDS;
      this.commandsChanged = true;
      for (const turn of [this.sent, this.live]) {
        if (turn?.state === "running") this.end(turn, "failed", "The agent stopped.");
      }
      this.sent = this.live = undefined;
      if (state.kind !== "ready") this.handBack();
      // A conversation went on before: the agent no longer remembers it.
      const last = this.entries.at(-1);
      if (state.kind === "ready" && last && last.kind !== "restart") {
        const divider: ChatEntry = { kind: "restart", id: this.nextId++ };
        this.entries.push(divider);
        this.dirtyHeads.add(divider.id);
      }
      this.changed();
    });
    // Messages sent before the session opened go once it is.
    session.onChange(() => this.next());
  }

  get current(): ChatState {
    return {
      entries: this.entries,
      busy: this.busy,
      queue: this.queued(),
      commands: this.commands,
    };
  }

  onPost(listener: (message: ChatMessage) => void): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }

  /** `/new` starts a new conversation; anything else goes, now or after the running turn. */
  send(text: string, images: Attachment[]): void {
    if (text.trim() === "/new" && images.length === 0) {
      this.newChat();
      return;
    }
    const message = { id: this.nextId++, text, images };
    if (this.busy || !this.session.sessionId) {
      this.queue.push(message);
      this.log.info(`chat: queued (${this.queue.length} waiting)`);
      this.changed();
      return;
    }
    void this.start(message);
  }

  /** Stops the running turn; the waiting messages go back to the message box. */
  cancel(): void {
    this.handBack();
    const sessionId = this.session.sessionId;
    if (!sessionId || !this.busy) return;
    this.log.info("chat: stop requested");
    this.agent.notify("session/cancel", { sessionId });
    // A turn the agent started answers no request: it ends here.
    if (this.live && this.live !== this.sent) this.end(this.live, "cancelled");
  }

  unqueue(id: number): void {
    this.queue = this.queue.filter((message) => message.id !== id);
    this.changed();
  }

  /** The agent restarts: a new process holds a new conversation. */
  newChat(): void {
    this.log.info("chat: new conversation");
    this.handBack();
    // Nothing the stopping process still sends lands in the new conversation.
    this.run++;
    this.turns.clear();
    this.streaming = this.thinking = undefined;
    this.entries = [];
    this.sent = this.live = undefined;
    this.post({ type: "chat", chat: this.current });
    void this.agent.restart();
  }

  private get busy(): boolean {
    return this.sent !== undefined || this.live !== undefined;
  }

  private async start({ id, text, images }: Queued): Promise<void> {
    const sessionId = this.session.sessionId;
    if (!sessionId) return;
    const run = this.run;
    const turn: Turn = {
      kind: "turn",
      id,
      prompt: { text, images: images.map(thumbOf) },
      state: "running",
      items: [],
    };
    this.entries.push(turn);
    this.sent = turn;
    this.dirtyHeads.add(id);
    this.changed();
    this.log.info(`chat: turn ${id} sent (${text.length} chars, ${count(images.length, "image")})`);
    const prompt = [
      { type: "text", text },
      ...images.map(({ mimeType, data }) => ({ type: "image", mimeType, data })),
    ];
    try {
      const { stopReason } = await this.agent.request<{ stopReason: string }>("session/prompt", { sessionId, prompt });
      if (run !== this.run) return;
      if (stopReason === "max_tokens" || stopReason === "max_turn_requests") {
        this.push(turn, { kind: "notice", tone: "warning", text: "The autonomous run reached its limit." });
      }
      this.end(turn, stopReason === "cancelled" ? "cancelled" : "done", undefined, stopReason);
    } catch (error) {
      if (run !== this.run) return;
      this.end(turn, "failed", errorText(error));
    }
  }

  private end(turn: Turn, state: Turn["state"], error?: string, reason?: string): void {
    if (this.thinking?.turn === turn) this.settle();
    if (turn.state === "running") {
      turn.state = state;
      if (error) turn.error = error;
      this.dirtyHeads.add(turn.id);
      const why = reason ? ` (${reason})` : "";
      this.log.info(`chat: turn ${turn.id} ${state}${why}${error ? `: ${error}` : ""}; ${summary(turn)}`);
    }
    if (this.sent === turn) this.sent = undefined;
    if (this.live === turn) this.live = undefined;
    this.streaming = undefined;
    this.next();
    this.changed();
  }

  /** The next waiting message, once nothing runs. */
  private next(): void {
    if (this.busy || !this.session.sessionId) return;
    const message = this.queue.shift();
    if (message) void this.start(message);
    else this.changed();
  }

  /** The waiting messages, back to the message box. */
  private handBack(): void {
    if (this.queue.length === 0) return;
    const text = this.queue
      .map((message) => message.text)
      .filter(Boolean)
      .join("\n\n");
    const images = this.queue.flatMap((message) => message.images);
    this.log.info(`chat: ${this.queue.length} waiting back in the message box`);
    this.queue = [];
    this.post({ type: "chatDraft", text, images });
    this.changed();
  }

  private onUpdate(params: unknown): void {
    const { sessionId, update } = params as { sessionId: string; update: Update };
    if (sessionId !== this.session.sessionId) return;
    const meta = update._meta?.unharnessed ?? {};
    if (update.sessionUpdate === "available_commands_update") {
      this.setCommands(update.availableCommands ?? []);
      return;
    }
    if (meta.turnStart) {
      this.begin(meta.promptTurnId ?? 0, meta.turnStart);
      return;
    }
    const turn = this.turns.get(meta.promptTurnId ?? 0) ?? this.live;
    if (!turn) return;
    switch (update.sessionUpdate) {
      case "agent_thought_chunk":
      case "agent_message_chunk": {
        const text = !Array.isArray(update.content) ? (update.content?.text ?? "") : "";
        const kind = update.sessionUpdate === "agent_thought_chunk" ? "thought" : "text";
        this.stream(turn, kind, `${kind}:${update.messageId}`, text);
        break;
      }
      case "tool_call":
        this.push(turn, {
          kind: "tool",
          id: update.toolCallId ?? "",
          title: update.title ?? "Tool",
          status: "running",
          input: toolInput(update.rawInput),
        });
        break;
      case "tool_call_update":
        this.toolDone(turn, update, meta);
        break;
      case "session_info_update":
        this.info(turn, meta);
        break;
    }
  }

  /** What started a turn comes first in it: our prompt, or a wake. */
  private begin(agentTurn: number, start: NonNullable<Meta["turnStart"]>): void {
    if (start.origin === "prompt" && this.sent) {
      this.turns.set(agentTurn, this.sent);
      this.live = this.sent;
      return;
    }
    const turn: Turn = { kind: "turn", id: this.nextId++, wake: start.wake, state: "running", items: [] };
    this.turns.set(agentTurn, turn);
    this.live = turn;
    // Our prompt waits for it: the agent's turn shows above it.
    const at = this.sent ? this.entries.indexOf(this.sent) : -1;
    if (at >= 0) {
      this.entries.splice(at, 0, turn);
      this.post({ type: "chat", chat: this.current });
    } else {
      this.entries.push(turn);
      this.dirtyHeads.add(turn.id);
    }
    this.log.info(`chat: turn ${turn.id} started by the agent (${start.wake ? WAKES[start.wake] : start.origin})`);
    this.changed();
  }

  private stream(turn: Turn, kind: "thought" | "text", key: string, text: string): void {
    if (!text) return;
    const last = turn.items.at(-1);
    if (last && this.streaming?.item === last && this.streaming.key === key && last.kind === kind) {
      last.text += text;
      this.dirtyItem(turn, turn.items.length - 1);
    } else {
      // Reasoning the agent did not say opened: timed from its first words.
      const item: TurnItem = kind === "thought" ? { kind, text, started: Date.now() } : { kind, text };
      this.push(turn, item);
      this.streaming = { item, key };
      if (kind === "thought") this.thinking = { turn, at: turn.items.length - 1 };
    }
    this.changed();
  }

  /** A reasoning block opens: it shows at once, before its words (if they ever come). */
  private think(turn: Turn, messageId: string): void {
    const item: TurnItem = { kind: "thought", text: "", started: Date.now() };
    this.push(turn, item);
    this.streaming = { item, key: `thought:${messageId}` };
    this.thinking = { turn, at: turn.items.length - 1 };
  }

  /** The reasoning under way is over: how long it took. */
  private settle(): void {
    const thinking = this.thinking;
    this.thinking = undefined;
    const item = thinking?.turn.items[thinking.at];
    if (!thinking || item?.kind !== "thought" || item.ms !== undefined) return;
    item.ms = Date.now() - item.started;
    this.dirtyItem(thinking.turn, thinking.at);
    const took = (item.ms / 1000).toFixed(1);
    this.log.info(`chat: turn ${thinking.turn.id} thought for ${took} s (${item.text.length} chars)`);
    this.changed();
  }

  private push(turn: Turn, item: TurnItem): void {
    // Whatever comes next, the reasoning before it is over.
    this.settle();
    turn.items.push(item);
    this.streaming = undefined;
    this.dirtyItem(turn, turn.items.length - 1);
    this.changed();
  }

  private toolDone(turn: Turn, update: Update, meta: Meta): void {
    const at = turn.items.findIndex((item) => item.kind === "tool" && item.id === update.toolCallId);
    if (at < 0) return;
    const tool = turn.items[at] as Tool;
    if (update.status === "completed") tool.status = "done";
    if (update.status === "failed") tool.status = "failed";
    if (Array.isArray(update.content)) {
      const text = update.content.map((block) => block.content?.text ?? "").join("\n");
      const cut = text.length - OUTPUT_LIMIT;
      tool.output = cut > 0 ? `${text.slice(0, OUTPUT_LIMIT)}\n… ${cut} more characters` : text;
    }
    if (meta.ipython?.attachments?.length) tool.images = meta.ipython.attachments.length;
    if (meta.ipython?.diffCount) tool.diffs = meta.ipython.diffCount;
    this.dirtyItem(turn, at);
    this.changed();
  }

  /** Reasoning opening and closing, a command's answer, a compaction, a goal's change; the end of a turn the agent started. */
  private info(turn: Turn, meta: Meta): void {
    if (meta.thinking?.state === "start") this.think(turn, meta.thinking.messageId);
    if (meta.thinking?.state === "end") this.settle();
    if (meta.command) {
      const { severity, text } = meta.command;
      const tone = severity === "error" || severity === "warning" ? severity : "info";
      this.push(turn, { kind: "notice", tone, text });
    }
    if (meta.compaction?.tokensBefore !== undefined) {
      const before = meta.compaction.tokensBefore.toLocaleString("en-US");
      this.push(turn, { kind: "notice", tone: "info", text: `Compacted the conversation (${before} tokens before).` });
    }
    if (meta.goal && meta.goal.status !== this.goal) {
      this.goal = meta.goal.status;
      const objective = meta.goal.objective ? `: ${meta.goal.objective}` : "";
      this.push(turn, { kind: "notice", tone: "info", text: `Goal ${meta.goal.status}${objective}` });
    }
    // Our prompt ends with its answer; a turn the agent started, here.
    if (meta.phase === "responseBoundary" && turn !== this.sent) {
      if (meta.outcome === "error") this.end(turn, "failed", "The turn failed.");
      else this.end(turn, "done");
    }
  }

  private setCommands(commands: NonNullable<Update["availableCommands"]>): void {
    const kinds = ["command", "prompt", "skill"] as const;
    const agent = commands.map(({ name, description, input, _meta }): ChatCommand => {
      const kind = kinds.find((k) => k === _meta?.unharnessed?.kind) ?? "command";
      return { name, description, hint: input?.hint || undefined, kind };
    });
    this.commands = [...PANEL_COMMANDS, ...agent.filter((command) => command.name !== "new")];
    this.commandsChanged = true;
    this.log.info(`chat: ${agent.length} commands: ${agent.map((command) => command.name).join(", ")}`);
    this.changed();
  }

  private dirtyItem(turn: Turn, at: number): void {
    const items = this.dirtyItems.get(turn.id) ?? new Set();
    this.dirtyItems.set(turn.id, items.add(at));
  }

  private queued(): QueuedMessage[] {
    return this.queue.map(({ id, text, images }) => ({ id, text, images: images.map(thumbOf) }));
  }

  /** What changed goes to the panel together, a moment later. */
  private changed(): void {
    this.timer ??= setTimeout(() => this.flush(), FLUSH_MS);
  }

  private flush(): void {
    this.timer = undefined;
    const byId = new Map(this.entries.map((entry) => [entry.id, entry]));
    const heads = [...this.dirtyHeads].flatMap((id): EntryHead[] => {
      const entry = byId.get(id);
      if (!entry) return [];
      if (entry.kind === "restart") return [entry];
      const { items: _, ...head } = entry;
      return [head];
    });
    const items: [number, number, TurnItem][] = [];
    for (const [id, indexes] of this.dirtyItems) {
      const entry = byId.get(id);
      if (entry?.kind !== "turn") continue;
      for (const at of indexes) items.push([id, at, entry.items[at]]);
    }
    const patch = { heads, items, busy: this.busy, queue: this.queued() };
    this.post({ type: "chatPatch", patch: this.commandsChanged ? { ...patch, commands: this.commands } : patch });
    this.dirtyHeads.clear();
    this.dirtyItems.clear();
    this.commandsChanged = false;
  }

  private post(message: ChatMessage): void {
    // A full post carries what the next patch would.
    if (message.type === "chat") {
      this.dirtyHeads.clear();
      this.dirtyItems.clear();
      this.commandsChanged = false;
    }
    for (const listener of this.listeners) listener(message);
  }
}

function thumbOf({ thumb, name }: Attachment): Thumb {
  return { thumb, name };
}

/** A Python cell's code; another tool's input as JSON. */
function toolInput(raw: unknown): string {
  if (raw && typeof raw === "object" && "code" in raw && typeof raw.code === "string") return raw.code;
  return raw === undefined ? "" : JSON.stringify(raw, null, 2);
}

/** `1 image`, `2 images`. */
function count(n: number, what: string): string {
  return `${n} ${what}${n === 1 ? "" : "s"}`;
}

/** For the log: `2 texts, 1 thought, 1 tool`. */
function summary(turn: Turn): string {
  const counts = new Map<string, number>();
  for (const item of turn.items) counts.set(item.kind, (counts.get(item.kind) ?? 0) + 1);
  const parts = [...counts].map(([kind, n]) => count(n, kind));
  return parts.length ? parts.join(", ") : "nothing streamed";
}
