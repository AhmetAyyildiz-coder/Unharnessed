import type * as vscode from "vscode";
import { errorText, type Agent } from "./agent";
import type { ConfigChange, ModelChoice, SessionConfig, SessionState } from "./protocol";

/** An ACP session config option, as the agent sends it (the parts we read). */
type ConfigOption = {
  id: string;
  type: string;
  currentValue: string | boolean;
  options?: (SelectValue | { group: string; options: SelectValue[] })[];
};

type SelectValue = {
  value: string;
  name: string;
  _meta?: { unharnessed?: { model?: ModelMeta } };
};

/** `_meta.unharnessed.model` on a `model` value (crates/uh-daemon/src/acp/meta.rs). */
type ModelMeta = {
  id: string;
  name: string;
  provider: string;
  contextWindow: number;
  input: string[];
  cost: { input: number; output: number };
};

/**
 * The agent session the panel works in: opened as soon as the agent is
 * ready, one per agent process. It holds the pickers (model, reasoning
 * effort, fast mode); the chat will run in it too.
 */
export class Session {
  private state: SessionState | null = null;
  private id: string | undefined;
  /** Bumped per agent process, so a late answer never lands on the next one. */
  private run = 0;
  private readonly listeners = new Set<() => void>();

  constructor(
    private readonly agent: Agent,
    private readonly cwd: string,
    private readonly log: vscode.LogOutputChannel,
  ) {
    agent.onNotification("session/update", (params) => this.onUpdate(params));
    agent.onChange((state) => {
      this.run++;
      this.id = undefined;
      this.set(state.kind === "ready" ? { kind: "opening" } : null);
      if (state.kind === "ready") void this.open();
    });
  }

  get current(): SessionState | null {
    return this.state;
  }

  onChange(listener: () => void): { dispose(): void } {
    this.listeners.add(listener);
    return { dispose: () => this.listeners.delete(listener) };
  }

  /**
   * Shown at once; the agent's answer then replaces it (the effort and fast
   * mode follow a model switch), or a refusal puts the previous one back.
   */
  async setOption(change: ConfigChange): Promise<void> {
    const state = this.state;
    if (state?.kind !== "open" || !this.id) return;
    const run = this.run;
    this.set({ kind: "open", config: withChange(state.config, change) });
    const params: Record<string, unknown> = { sessionId: this.id, configId: change.configId, value: change.value };
    if (typeof change.value === "boolean") params.type = "boolean";
    try {
      const result = await this.agent.request<{ configOptions: ConfigOption[] }>("session/set_config_option", params);
      if (run !== this.run) return;
      const config = readConfig(result.configOptions);
      this.log.info(`session: ${change.configId} set, ${describe(config)}`);
      this.set({ kind: "open", config });
    } catch (error) {
      if (run !== this.run) return;
      this.log.warn(`session: ${change.configId} refused: ${errorText(error)}`);
      this.set({ kind: "open", config: state.config, error: errorText(error) });
    }
  }

  private async open(): Promise<void> {
    const run = this.run;
    try {
      const result = await this.agent.request<{ sessionId: string; configOptions?: ConfigOption[] }>("session/new", {
        cwd: this.cwd,
        mcpServers: [],
      });
      if (run !== this.run) return;
      this.id = result.sessionId;
      const config = readConfig(result.configOptions ?? []);
      this.log.info(`session: opened, ${describe(config)}`);
      this.set({ kind: "open", config });
    } catch (error) {
      if (run !== this.run) return;
      this.log.warn(`session: not opened: ${errorText(error)}`);
      this.set({ kind: "failed", error: errorText(error) });
    }
  }

  /** The agent republishes the pickers after a login, a catalog refresh or a turn. */
  private onUpdate(params: unknown): void {
    const { sessionId, update } = params as {
      sessionId: string;
      update: { sessionUpdate: string; configOptions?: ConfigOption[] };
    };
    if (sessionId !== this.id || update.sessionUpdate !== "config_option_update" || this.state?.kind !== "open") return;
    const config = readConfig(update.configOptions ?? []);
    this.log.info(`session: options updated, ${describe(config)}`);
    this.set({ kind: "open", config });
  }

  private set(state: SessionState | null): void {
    this.state = state;
    for (const listener of this.listeners) listener();
  }
}

function readConfig(options: ConfigOption[]): SessionConfig {
  const model = options.find((option) => option.id === "model" && option.type === "select");
  const effort = options.find((option) => option.id === "thought_level" && option.type === "select");
  const fast = options.find((option) => option.id === "fast_mode" && option.type === "boolean");
  return {
    model: model ? { current: String(model.currentValue), choices: values(model).map(choice) } : null,
    effort: effort ? { current: String(effort.currentValue), levels: values(effort).map((v) => v.value) } : null,
    fast: fast ? fast.currentValue === true : null,
  };
}

/** A select's values, groups flattened. */
function values(option: ConfigOption): SelectValue[] {
  return (option.options ?? []).flatMap((entry) => ("group" in entry ? entry.options : [entry]));
}

function choice(value: SelectValue): ModelChoice {
  const meta = value._meta?.unharnessed?.model;
  if (meta) {
    const { input, output } = meta.cost;
    return {
      value: value.value,
      id: meta.id,
      name: meta.name,
      provider: meta.provider,
      contextWindow: meta.contextWindow,
      input: meta.input,
      cost: { input, output },
    };
  }
  // Without details, the value is the `["provider", "id"]` pair.
  const [provider = "", id = value.value] = parsePair(value.value);
  return { value: value.value, id, name: value.name, provider, contextWindow: 0, input: [] };
}

function parsePair(value: string): string[] {
  try {
    const pair: unknown = JSON.parse(value);
    return Array.isArray(pair) ? pair.map(String) : [];
  } catch {
    return [];
  }
}

function withChange(config: SessionConfig, change: ConfigChange): SessionConfig {
  switch (change.configId) {
    case "model":
      return config.model ? { ...config, model: { ...config.model, current: change.value } } : config;
    case "thought_level":
      return config.effort ? { ...config, effort: { ...config.effort, current: change.value } } : config;
    case "fast_mode":
      return { ...config, fast: change.value };
  }
}

/** For the log: `model openai/gpt-5.4 (44 choices), effort medium, fast off`. */
function describe(config: SessionConfig): string {
  const current = config.model?.choices.find((c) => c.value === config.model?.current);
  const model = current ? `${current.provider}/${current.id}` : (config.model?.current ?? "none");
  const parts = [`model ${model} (${config.model?.choices.length ?? 0} choices)`];
  parts.push(`effort ${config.effort?.current ?? "none"}`);
  parts.push(`fast ${config.fast === null ? "none" : config.fast ? "on" : "off"}`);
  return parts.join(", ");
}
