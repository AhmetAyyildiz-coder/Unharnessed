import { useEffect, useState, type SubmitEvent } from "react";
import type { Provider } from "../src/protocol";
import type { ProviderResult } from "./App";
import { IconBack, IconChevron, IconSearch, IconUnlink } from "./icons";
import { post } from "./vscode";

/** From this many rows on, a search field filters them. */
const SEARCH_FROM = 9;

/** The row whose key form or disconnect confirmation is open. */
type Open = { id: string; method: Provider["method"]; mode: "key" | "disconnect" };

function methodLabel(provider: Provider): string {
  return provider.method === "subscription" ? "Subscription" : "API key";
}

/**
 * Every provider the agent knows, in three groups: connected, subscriptions
 * (a browser sign-in) and API keys (a key pasted here). Without `onBack` it
 * is the first screen: nothing is connected yet.
 */
export function Providers({
  providers,
  result,
  onBack,
}: {
  providers: Provider[];
  result: ProviderResult | null;
  onBack?: () => void;
}) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<Open | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  // The host answers each key save or disconnect with a result.
  useEffect(() => {
    if (!result) return;
    setBusy(false);
    if (result.error) setError(result.error);
    else {
      setOpen(null);
      setError(undefined);
    }
  }, [result]);

  const toggle = (next: Open) => {
    const same = open?.id === next.id && open.method === next.method && open.mode === next.mode;
    setOpen(same ? null : next);
    setBusy(false);
    setError(undefined);
  };
  const run = (message: Parameters<typeof post>[0]) => {
    setBusy(true);
    setError(undefined);
    post(message);
  };

  const words = query.trim().toLowerCase();
  const shown = words ? providers.filter((p) => `${p.name} ${p.id}`.toLowerCase().includes(words)) : providers;
  const groups: [string, Provider[]][] = [
    ["Connected", shown.filter((p) => p.connected)],
    ["Subscriptions", shown.filter((p) => !p.connected && p.method === "subscription")],
    ["API keys", shown.filter((p) => !p.connected && p.method === "apiKey")],
  ];

  return (
    <div className="screen">
      <header className="topbar">
        <div className="topbar-title">
          {onBack && (
            <button className="icon-button" type="button" title="Back" aria-label="Back" onClick={onBack}>
              <IconBack />
            </button>
          )}
          <span>Providers</span>
        </div>
      </header>
      <main className="content">
        {!onBack && (
          <div className="intro">
            <h1>Connect a provider</h1>
            <p>Sign in with a subscription, or add an API key.</p>
          </div>
        )}
        {providers.length >= SEARCH_FROM && (
          <label className="search">
            <IconSearch />
            <input
              className="input"
              type="search"
              placeholder="Search providers"
              aria-label="Search providers"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
            />
          </label>
        )}
        {groups.map(
          ([label, rows]) =>
            rows.length > 0 && (
              <section className="section" key={label}>
                <h2 className="section-label">{label}</h2>
                <ul className="list">
                  {rows.map((provider) => {
                    const mode = open?.id === provider.id && open.method === provider.method ? open.mode : null;
                    return (
                      <Row
                        key={`${provider.id}/${provider.method}`}
                        provider={provider}
                        mode={mode}
                        busy={busy}
                        error={mode ? error : undefined}
                        onToggle={(mode) => toggle({ id: provider.id, method: provider.method, mode })}
                        onRun={run}
                      />
                    );
                  })}
                </ul>
              </section>
            ),
        )}
        {shown.length === 0 && <p className="note">No provider matches “{query.trim()}”.</p>}
      </main>
    </div>
  );
}

function Row({
  provider,
  mode,
  busy,
  error,
  onToggle,
  onRun,
}: {
  provider: Provider;
  mode: Open["mode"] | null;
  busy: boolean;
  error?: string;
  onToggle: (mode: Open["mode"]) => void;
  onRun: (message: Parameters<typeof post>[0]) => void;
}) {
  const browser = provider.login === "browser";
  // In the connected group the method tells the two Anthropic rows apart.
  const meta = provider.connected ? (provider.note ?? methodLabel(provider)) : provider.note;
  const activate = () => (browser ? post({ type: "login", providerId: provider.id }) : onToggle("key"));

  return (
    <li className="row">
      <div className="row-line">
        <button
          className={`row-body is-button${mode === "key" ? " is-open" : ""}`}
          type="button"
          aria-expanded={browser ? undefined : mode === "key"}
          onClick={activate}
        >
          {provider.connected && <span className="dot" />}
          <span className="row-main">
            <span className="row-name">{provider.name}</span>
            {meta && <span className="row-meta">{meta}</span>}
          </span>
          <IconChevron className="row-end" />
        </button>
        {provider.removable && (
          <button
            className="icon-button"
            type="button"
            title="Disconnect"
            aria-label={`Disconnect ${provider.name}`}
            onClick={() => onToggle("disconnect")}
          >
            <IconUnlink />
          </button>
        )}
      </div>
      {provider.warning && <p className="row-warning">{provider.warning}</p>}
      {mode === "key" && (
        <KeyForm
          provider={provider}
          busy={busy}
          error={error}
          onSave={(apiKey) => onRun({ type: "setApiKey", providerId: provider.id, apiKey })}
          onCancel={() => onToggle("key")}
        />
      )}
      {mode === "disconnect" && (
        <div className="row-form">
          <p className="note">
            {provider.method === "subscription"
              ? `Sign out of ${provider.name}?`
              : `Remove the saved ${provider.name} API key?`}
          </p>
          {error && <p className="error">{error}</p>}
          <div className="actions">
            <button
              className="button is-danger"
              type="button"
              disabled={busy}
              onClick={() => onRun({ type: "logout", providerId: provider.id })}
            >
              Disconnect
            </button>
            <button className="button is-quiet" type="button" onClick={() => onToggle("disconnect")}>
              Cancel
            </button>
          </div>
        </div>
      )}
    </li>
  );
}

function KeyForm({
  provider,
  busy,
  error,
  onSave,
  onCancel,
}: {
  provider: Provider;
  busy: boolean;
  error?: string;
  onSave: (apiKey: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState("");
  const empty = value.trim() === "";
  const submit = (event: SubmitEvent) => {
    event.preventDefault();
    if (!empty && !busy) onSave(value);
  };

  return (
    <form className="row-form" onSubmit={submit} onKeyDown={(event) => event.key === "Escape" && onCancel()}>
      <input
        className="input is-secret"
        type="password"
        placeholder={`${provider.name} API key`}
        aria-label={`${provider.name} API key`}
        autoComplete="off"
        spellCheck={false}
        autoFocus
        disabled={busy}
        value={value}
        onChange={(event) => setValue(event.target.value)}
      />
      {error && <p className="error">{error}</p>}
      <div className="actions">
        <button className="button" type="submit" disabled={empty || busy}>
          {busy ? "Saving" : "Save"}
        </button>
        <button className="button is-quiet" type="button" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}
