import { useCallback, useEffect, useState } from "react";
import type { AgentState, HostMessage, LoginState, Provider, Screen } from "../src/protocol";
import { Providers } from "./Providers";
import { SignIn } from "./SignIn";
import { Splash } from "./Splash";
import { Status } from "./Status";
import { onHostMessage, post } from "./vscode";

type Phase = "boot" | "splash" | "main";
export type ProviderResult = Extract<HostMessage, { type: "providerResult" }>;

export function App() {
  const [phase, setPhase] = useState<Phase>("boot");
  const [agent, setAgent] = useState<AgentState | null>(null);
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [login, setLogin] = useState<LoginState | null>(null);
  const [result, setResult] = useState<ProviderResult | null>(null);
  const [managing, setManaging] = useState(false);
  const splashDone = useCallback(() => setPhase("main"), []);

  useEffect(() => {
    const off = onHostMessage((message) => {
      switch (message.type) {
        case "init":
          setAgent(message.agent);
          setProviders(message.providers);
          setLogin(message.login);
          setPhase(message.splash ? "splash" : "main");
          break;
        case "agent":
          setAgent(message.agent);
          break;
        case "providers":
          setProviders(message.providers);
          setLogin(message.login);
          break;
        case "providerResult":
          setResult(message);
          break;
      }
    });
    post({ type: "ready" });
    return off;
  }, []);

  // Nothing connected yet: the providers screen is the way in.
  const onboarding = providers !== null && !providers.some((p) => p.connected);
  let screen: Screen | null = null;
  if (phase === "splash") screen = "splash";
  else if (phase === "main" && agent) {
    if (agent.kind === "ready" && login) screen = "login";
    else if (agent.kind === "ready" && (managing || onboarding)) screen = "providers";
    else screen = "status";
  }

  useEffect(() => {
    if (screen) post({ type: "screen", screen });
  }, [screen]);

  if (!screen || !agent) return null;
  switch (screen) {
    case "splash":
      return <Splash agent={agent} onDone={splashDone} />;
    case "login":
      return login && <SignIn login={login} />;
    case "providers":
      return (
        <Providers
          providers={providers ?? []}
          result={result}
          onBack={onboarding ? undefined : () => setManaging(false)}
        />
      );
    default:
      return <Status agent={agent} onProviders={() => setManaging(true)} />;
  }
}
