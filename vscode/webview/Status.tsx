import type { AgentState } from "../src/protocol";
import { IconCross, IconRestart } from "./icons";
import { post } from "./vscode";
import { Wordmark } from "./Wordmark";

type Failed = Extract<AgentState, { kind: "failed" }>;

const LABELS: Record<AgentState["kind"], string> = {
  starting: "Starting agent",
  ready: "Agent running",
  failed: "Agent stopped",
};

const restart = () => post({ type: "restartAgent" });

export function Status({ agent }: { agent: AgentState }) {
  return (
    <div className="screen">
      <header className="topbar">
        <Wordmark />
        <button
          className="icon-button"
          type="button"
          title="Restart agent"
          aria-label="Restart agent"
          disabled={agent.kind === "starting"}
          onClick={restart}
        >
          <IconRestart />
        </button>
      </header>
      <main className="content">
        <div className={`agent-line is-${agent.kind}`}>
          {agent.kind === "failed" ? <IconCross className="agent-mark" /> : <span className="agent-mark" />}
          <span>{LABELS[agent.kind]}</span>
        </div>
        {agent.kind === "failed" && <Failure agent={agent} />}
      </main>
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
