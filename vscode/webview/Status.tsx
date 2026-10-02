import type { AgentState, Provider, SessionState } from "../src/protocol";
import { Composer } from "./Composer";
import { IconCross, IconPlug, IconRestart } from "./icons";
import { post } from "./vscode";
import { Wordmark } from "./Wordmark";

type Failed = Extract<AgentState, { kind: "failed" }>;

const LABELS: Record<AgentState["kind"], string> = {
  starting: "Starting agent",
  ready: "Agent running",
  failed: "Agent stopped",
};

const restart = () => post({ type: "restartAgent" });

/** Home, where the chat will be: the agent's state, and the message box with the model pickers. */
export function Status({
  agent,
  session,
  providers,
  onProviders,
}: {
  agent: AgentState;
  session: SessionState | null;
  providers: Provider[];
  onProviders: () => void;
}) {
  return (
    <div className="screen">
      <header className="topbar">
        <Wordmark />
        <div className="topbar-actions">
          <button
            className="icon-button"
            type="button"
            aria-label="Providers"
            data-tip="Providers"
            data-tip-side="below"
            disabled={agent.kind !== "ready"}
            onClick={onProviders}
          >
            <IconPlug />
          </button>
          <button
            className="icon-button"
            type="button"
            aria-label="Restart agent"
            data-tip="Restart agent"
            data-tip-side="below"
            data-tip-align="end"
            disabled={agent.kind === "starting"}
            onClick={restart}
          >
            <IconRestart />
          </button>
        </div>
      </header>
      <main className="content">
        <div className={`agent-line is-${agent.kind}`}>
          {agent.kind === "failed" ? <IconCross className="agent-mark" /> : <span className="agent-mark" />}
          <span>{LABELS[agent.kind]}</span>
        </div>
        {agent.kind === "failed" && <Failure agent={agent} />}
      </main>
      {agent.kind === "ready" && <Composer session={session} providers={providers} />}
    </div>
  );
}

function Failure({ agent }: { agent: Failed }) {
  return (
    <>
      <p className="error">{agent.error}</p>
      {agent.stderr.length > 0 && <pre className="log">{agent.stderr.join("\n")}</pre>}
      <button className="button" type="button" onClick={restart}>
        <IconRestart />
        Restart
      </button>
    </>
  );
}
