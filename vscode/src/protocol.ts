// Messages between the extension host and the panel webview. Types only:
// both bundles import this file.

/**
 * The `unharnessed --mode acp` process as the panel shows it. Versions, pid
 * and binary path stay in the log: the panel only says whether it runs.
 */
export type AgentState =
  | { kind: "starting" }
  | { kind: "ready" }
  | { kind: "failed"; error: string; stderr: string[] };

/** One provider row, as the agent's `_unharnessed/providers/list` gives it. */
export type Provider = {
  id: string;
  name: string;
  /** What the credential is. Anthropic and xAI have a row of each. */
  method: "subscription" | "apiKey";
  /** `browser`: the agent runs the sign-in. `apiKey`: the panel asks for the key. */
  login: "browser" | "apiKey";
  connected: boolean;
  /** The credential is in the agent's auth.json, so it can be removed. */
  removable: boolean;
  /** Where an outside key comes from, or why the row is not connected. */
  note?: string;
  /** The Anthropic subscription warning, on the row that uses it. */
  warning?: string;
};

/** One step of a running sign-in (`_unharnessed/providers/login_update`). */
export type LoginUpdate =
  | { kind: "progress"; message: string }
  | { kind: "waiting"; message: string }
  | { kind: "url"; url: string; instructions: string | null }
  | {
      kind: "prompt";
      promptId: number;
      message: string;
      secret: boolean;
      allowEmpty: boolean;
      /** An alternative to the browser sign-in running beside it. */
      optional: boolean;
    }
  | { kind: "team"; promptId: number; teams: Team[]; current: string | null };

export type Team = { id: string; name: string; role: string | null };

/** What a prompt gets back. */
export type LoginAnswer =
  | { kind: "text"; value: string }
  /** `null`: the personal account. */
  | { kind: "team"; teamId: string | null }
  /** On a team picker: keep the stored team. */
  | { kind: "dismiss" };

/** The browser sign-in in progress, as the panel shows it. */
export type LoginState = {
  providerId: string;
  name: string;
  url?: { url: string; instructions: string | null };
  /** The latest progress line; `waiting` while the provider waits on the browser. */
  status?: { message: string; waiting: boolean };
  prompt?: Extract<LoginUpdate, { kind: "prompt" }>;
  team?: Extract<LoginUpdate, { kind: "team" }>;
  /** Set when the sign-in failed; the screen stays until the user leaves it. */
  error?: string;
};

/** One model the session can switch to: a value of the `model` option. */
export type ModelChoice = {
  /** The agent's opaque value, sent back to select it. */
  value: string;
  id: string;
  name: string;
  /** The provider id (`openai`); the panel names it from the providers list. */
  provider: string;
  /** Tokens; 0 when the agent did not say. */
  contextWindow: number;
  /** What the model reads: `text`, `image`. */
  input: string[];
  /** Dollars per million tokens; absent when the agent did not say. */
  cost?: { input: number; output: number };
};

/** The session's pickers: its config options, kept current by the agent. */
export type SessionConfig = {
  model: { current: string; choices: ModelChoice[] } | null;
  /** The reasoning effort; `null` when the model has none to pick. */
  effort: { current: string; levels: string[] } | null;
  /** The fast mode (priority tier); `null` when the model has none. */
  fast: boolean | null;
};

/** The agent session (`session/new`) as the panel shows it. */
export type SessionState =
  | { kind: "opening" }
  /** `error`: the last selection the agent refused. */
  | { kind: "open"; config: SessionConfig; error?: string }
  | { kind: "failed"; error: string };

/** The options the panel sets. */
export type ConfigChange =
  | { configId: "model" | "thought_level"; value: string }
  | { configId: "fast_mode"; value: boolean };

/** The global settings, as `_unharnessed/settings/get` gives them. Keys are the file's dotted paths. */
export type SettingsView = {
  /** Each key's value in `~/.unharnessed/settings.json`; `null` when unset. */
  values: Record<string, unknown>;
  /** What the agent uses for an unset key; `null`: nothing. */
  defaults: Record<string, unknown>;
  /** Keys read when the agent starts; the others apply on their next use. */
  restart: string[];
};

/** One MCP server, as the agent's `_unharnessed/mcp/list` gives it. */
export type McpServer = {
  id: string;
  label: string;
  /** `catalog`: a service the agent knows (Linear, Notion); `user`: one of the `mcpServers` setting. */
  source: "catalog" | "user";
  /** `pending`: signed in, the connection not checked yet. */
  status: "connected" | "pending" | "not_connected" | "setup_required" | "disabled" | "error";
  /** An OAuth sign-in can run now. */
  signIn: boolean;
  /** A sign-in or token is stored: signing out removes it. */
  signedIn: boolean;
  description?: string;
  /** What it needs, or why it fails. */
  hint?: string;
};

/** One skill, as the agent's `_unharnessed/skills/list` gives it. */
export type Skill = {
  name: string;
  description: string;
  /** Its file. */
  path: string;
  /** Its folder, `~` for the home folder. */
  location: string;
  /** `user`: the agent's skills folders; `folder`: a folder added here; `project`: the workspace's. */
  source: "builtin" | "user" | "folder" | "project";
  /** The agent loads it when it starts. */
  enabled: boolean;
  /** The running agent has it. */
  active: boolean;
};

export type SkillsView = {
  skills: Skill[];
  /** The folders added to the `skills` setting. */
  folders: string[];
  /** What keeps a skill from loading. */
  problems: { message: string; path?: string }[];
  /** The next start loads other skills than the running agent has. */
  restart: boolean;
};

/** The settings screen's state. */
export type SettingsState = {
  /** `null` until the agent has answered. */
  view: SettingsView | null;
  /** What a restart would apply: the restart keys changed since the agent started, `skills`, `mcp`. */
  pending: string[];
  mcp: McpServer[] | null;
  skills: SkillsView | null;
};

export type HostMessage =
  /** Answer to `ready`; `splash` is true once per window. */
  | {
      type: "init";
      splash: boolean;
      agent: AgentState;
      providers: Provider[] | null;
      login: LoginState | null;
      session: SessionState | null;
      settings: SettingsState;
    }
  | { type: "agent"; agent: AgentState }
  /** `null` while the agent is not running. */
  | { type: "session"; session: SessionState | null }
  /** `providers` is `null` until the agent has listed them. */
  | { type: "providers"; providers: Provider[] | null; login: LoginState | null }
  /** How a saved key or a disconnect went; `error` is shown on the row. */
  | { type: "providerResult"; action: "apiKey" | "logout"; providerId: string; error?: string }
  | { type: "settings"; settings: SettingsState }
  /**
   * How a settings write went; `error` is shown under the setting. MCP and
   * skills actions answer under `mcp:<id>`, `skill:<path>`, `skills/create`
   * and `skills/folder`.
   */
  | { type: "settingResult"; key: string; error?: string };

export type WebviewMessage =
  | { type: "ready" }
  /** The screen now on display, for the log. */
  | { type: "screen"; screen: Screen }
  | { type: "restartAgent" }
  | { type: "setApiKey"; providerId: string; apiKey: string }
  | { type: "logout"; providerId: string }
  | { type: "login"; providerId: string }
  | { type: "loginAnswer"; promptId: number; answer: LoginAnswer }
  | { type: "loginCancel" }
  /** Leave a failed sign-in's screen. */
  | { type: "loginClose" }
  /** Open the running sign-in's page in the browser. */
  | { type: "loginOpen" }
  | ({ type: "setConfig" } & ConfigChange)
  /** `null` removes the key: its default applies again. */
  | { type: "setSetting"; key: string; value: unknown }
  | { type: "mcpSignIn"; id: string; label: string }
  | { type: "mcpSignOut"; id: string }
  | { type: "setSkill"; path: string; enabled: boolean }
  /** The new skill's file opens once it is written. */
  | { type: "createSkill"; name: string; description: string }
  | { type: "skillFolder"; path: string; add: boolean }
  /** Open a skill's file in the editor. */
  | { type: "openFile"; path: string }
  | { type: "copy"; text: string };

export type Screen = "splash" | "status" | "providers" | "login" | "settings";
