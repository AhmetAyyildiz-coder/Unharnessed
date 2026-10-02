import { useCallback, useEffect, useState } from "react";
import type { AgentState } from "../src/protocol";
import { Splash } from "./Splash";
import { Status } from "./Status";
import { onHostMessage, post } from "./vscode";

type Phase = "boot" | "splash" | "main";

export function App() {
  const [phase, setPhase] = useState<Phase>("boot");
  const [agent, setAgent] = useState<AgentState | null>(null);
  const splashDone = useCallback(() => setPhase("main"), []);

  useEffect(() => {
    const off = onHostMessage((message) => {
      setAgent(message.agent);
      if (message.type === "init") setPhase(message.splash ? "splash" : "main");
    });
    post({ type: "ready" });
    return off;
  }, []);

  useEffect(() => {
    if (phase !== "boot") post({ type: "screen", screen: phase === "splash" ? "splash" : "status" });
  }, [phase]);

  if (phase === "boot" || !agent) return null;
  if (phase === "splash") return <Splash agent={agent} onDone={splashDone} />;
  return <Status agent={agent} />;
}
