import type { AgentState, ChatState, Provider, SessionState } from "../src/protocol";
import { Transcript } from "./Chat";
import { Composer, type Draft, type ImageSettings } from "./Composer";
import { IconCross, IconNewChat, IconPlug, IconRestart, IconSliders } from "./icons";
import { post } from "./vscode";
import { Wordmark } from "./Wordmark";

type Failed = Extract<AgentState, { kind: "failed" }>;

const LABELS: Record<AgentState["kind"], string> = {
  starting: "Starting agent",
  ready: "Agent running",
  failed: "Agent stopped",
};

const restart = () => post({ type: "restartAgent" });

/** Home: the conversation, the agent's state while it has none, and the message box. */
export function Status({
  agent,
  session,
  providers,
  chat,
  images,
  draft,
  onDraft,
  onProviders,
  onSettings,
}: {
  agent: AgentState;
  session: SessionState | null;
  providers: Provider[];
  chat: ChatState;
  images: ImageSettings;
  draft: Draft;
  onDraft: (update: (draft: Draft) => Draft) => void;
  onProviders: () => void;
  onSettings: () => void;
}) {
  const empty = chat.entries.length === 0;
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
            aria-label="Settings"
            data-tip="Settings"
            data-tip-side="below"
            disabled={agent.kind !== "ready"}
            onClick={onSettings}
          >
            <IconSliders />
          </button>
          <button
            className="icon-button"
            type="button"
            aria-label="New conversation"
            data-tip="New conversation"
            data-tip-side="below"
            data-tip-align="end"
            disabled={agent.kind === "starting"}
            onClick={() => post({ type: "newChat" })}
          >
            <IconNewChat />
          </button>
        </div>
      </header>
      <Transcript chat={chat}>
        {(empty || agent.kind !== "ready") && (
          <div className={`agent-line is-${agent.kind}`}>
            {agent.kind === "failed" ? <IconCross className="agent-mark" /> : <span className="agent-mark" />}
            <span>{LABELS[agent.kind]}</span>
          </div>
        )}
        {agent.kind === "failed" && <Failure agent={agent} />}
      </Transcript>
      {agent.kind === "ready" && (
        <Composer
          session={session}
          providers={providers}
          chat={chat}
          images={images}
          draft={draft}
          onDraft={onDraft}
        />
      )}
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
