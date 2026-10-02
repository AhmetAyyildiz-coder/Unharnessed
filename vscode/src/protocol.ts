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

export type HostMessage =
  /** Answer to `ready`; `splash` is true once per window. */
  | { type: "init"; splash: boolean; agent: AgentState }
  | { type: "agent"; agent: AgentState };

export type WebviewMessage =
  | { type: "ready" }
  /** The screen now on display, for the log. */
  | { type: "screen"; screen: "splash" | "status" }
  | { type: "restartAgent" };
