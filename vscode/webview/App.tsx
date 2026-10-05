import { useCallback, useEffect, useState } from "react";
import type {
  AgentState,
  ChatState,
  HostMessage,
  LoginState,
  Provider,
  Screen,
  SessionState,
  SettingsState,
} from "../src/protocol";
import { applyPatch, EMPTY_CHAT } from "./Chat";
import type { Draft, ImageSettings } from "./Composer";
import { Providers } from "./Providers";
import { Settings, type SettingsTab } from "./Settings";
import { SignIn } from "./SignIn";
import { Splash } from "./Splash";
import { Status } from "./Status";
import { onHostMessage, post, saveDraft, savedDraft } from "./vscode";

type Phase = "boot" | "splash" | "main";
export type ProviderResult = Extract<HostMessage, { type: "providerResult" }>;
export type SettingResult = Extract<HostMessage, { type: "settingResult" }>;

export function App() {
  const [phase, setPhase] = useState<Phase>("boot");
  const [agent, setAgent] = useState<AgentState | null>(null);
  const [providers, setProviders] = useState<Provider[] | null>(null);
  const [login, setLogin] = useState<LoginState | null>(null);
  const [session, setSession] = useState<SessionState | null>(null);
  const [result, setResult] = useState<ProviderResult | null>(null);
  const [settings, setSettings] = useState<SettingsState>({ view: null, pending: [], mcp: null, skills: null });
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");
  const [settingResult, setSettingResult] = useState<SettingResult | null>(null);
  const [chat, setChat] = useState<ChatState>(EMPTY_CHAT);
  const [draft, setDraft] = useState<Draft>(() => ({ text: savedDraft(), images: [] }));
  const [managing, setManaging] = useState(false);
  const [tuning, setTuning] = useState(false);
  const splashDone = useCallback(() => setPhase("main"), []);
  useEffect(() => saveDraft(draft.text), [draft.text]);

  useEffect(() => {
    const off = onHostMessage((message) => {
      switch (message.type) {
        case "init":
          setAgent(message.agent);
          setProviders(message.providers);
          setLogin(message.login);
          setSession(message.session);
          setSettings(message.settings);
          setChat(message.chat);
          setPhase(message.splash ? "splash" : "main");
          break;
        case "chat":
          setChat(message.chat);
          break;
        case "chatPatch":
          setChat((chat) => applyPatch(chat, message.patch));
          break;
        case "chatDraft": {
          // The waiting messages come first: they were written before.
          const { text, images } = message;
          setDraft((draft) => ({
            text: [text, draft.text].filter(Boolean).join("\n\n"),
            images: [...images, ...draft.images],
          }));
          break;
        }
        case "agent":
          setAgent(message.agent);
          break;
        case "session":
          setSession(message.session);
          break;
        case "providers":
          setProviders(message.providers);
          setLogin(message.login);
          break;
        case "providerResult":
          setResult(message);
          break;
        case "settings":
          setSettings(message.settings);
          break;
        case "settingResult":
          setSettingResult(message);
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
    // It stays while a restart it asked for runs.
    else if (tuning && agent.kind !== "failed") screen = "settings";
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
    case "settings":
      return (
        <Settings
          settings={settings}
          ready={agent.kind === "ready"}
          session={session}
          providers={providers ?? []}
          result={settingResult}
          tab={settingsTab}
          onTab={setSettingsTab}
          onBack={() => setTuning(false)}
        />
      );
    default:
      return (
        <Status
          agent={agent}
          session={session}
          providers={providers ?? []}
          chat={chat}
          images={imageSettings(settings)}
          draft={draft}
          onDraft={setDraft}
          onProviders={() => setManaging(true)}
          onSettings={() => setTuning(true)}
        />
      );
  }
}

/** The image settings as the agent applies them: the file's value, else the agent's default. */
function imageSettings({ view }: SettingsState): ImageSettings {
  const value = (key: string) => view?.values[key] ?? view?.defaults[key] ?? null;
  return {
    block: value("images.blockImages") === true,
    resize: value("images.autoResize") !== false,
    imageModel: typeof value("imageModel") === "string" ? (value("imageModel") as string) : null,
  };
}
