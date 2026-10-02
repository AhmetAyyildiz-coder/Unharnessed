import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import type { ConfigChange, ModelChoice, Provider, SessionConfig, SessionState } from "../src/protocol";
import { IconBolt, IconCheck, IconChevronDown, IconImage, IconSearch, IconSend, IconText } from "./icons";
import { ProviderLogo } from "./logos";
import { post } from "./vscode";

const EFFORT_LABELS: Record<string, string> = {
  off: "Off",
  minimal: "Minimal",
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra high",
  max: "Max",
};

const effortLabel = (level: string) => EFFORT_LABELS[level] ?? level;

const PLACEHOLDER = "Ask Unharnessed";

const set = (change: ConfigChange) => post({ type: "setConfig", ...change });

type Menu = "model" | "effort";

/**
 * The message box at the bottom of home, with the session's pickers: the
 * model, its reasoning effort and its fast mode. Sending comes with the chat.
 */
export function Composer({ session, providers }: { session: SessionState | null; providers: Provider[] }) {
  const [menu, setMenu] = useState<Menu | null>(null);
  const [draft, setDraft] = useState("");
  /** Bumped when the empty box takes focus: the placeholder streams in again. */
  const [stream, setStream] = useState(0);
  const config = session?.kind === "open" ? session.config : null;
  const toggle = (next: Menu) => setMenu((open) => (open === next ? null : next));
  const close = () => setMenu(null);

  // A menu whose picker left (a model switch drops the effort) closes.
  useEffect(() => {
    if (menu === "model" && !config?.model) setMenu(null);
    if (menu === "effort" && !config?.effort) setMenu(null);
  }, [menu, config]);

  return (
    <div className="composer-shell">
      <div className="composer-glow" aria-hidden="true" />
      <div className="composer">
        {menu === "model" && config?.model && <ModelMenu model={config.model} providers={providers} onClose={close} />}
        <div className="composer-field">
          <textarea
            className="composer-input"
            rows={3}
            aria-label={PLACEHOLDER}
            spellCheck={false}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onFocus={() => draft === "" && setStream((n) => n + 1)}
          />
          {draft === "" && <Placeholder key={stream} streaming={stream > 0} />}
        </div>
        {session?.kind === "open" && session.error && <p className="composer-error">{session.error}</p>}
        <div className="composer-bar">
          <ModelButton
            session={session}
            providers={providers}
            open={menu === "model"}
            onClick={() => toggle("model")}
          />
          {config?.effort && (
            <EffortPicker
              effort={config.effort}
              open={menu === "effort"}
              onToggle={() => toggle("effort")}
              onClose={close}
            />
          )}
          {config && config.fast !== null && <FastToggle on={config.fast} />}
          <span className="composer-spacer" />
          <button className="send" type="button" aria-label="Send" data-tip="Send" data-tip-align="end" disabled>
            <IconSend />
          </button>
        </div>
      </div>
    </div>
  );
}

/** The box's placeholder; on focus it streams in again, letter by letter. */
function Placeholder({ streaming }: { streaming: boolean }) {
  return (
    <span className={streaming ? "composer-placeholder is-streaming" : "composer-placeholder"} aria-hidden="true">
      {[...PLACEHOLDER].map((char, i) => (
        <span key={i} style={streaming ? { animationDelay: `${i * 28}ms` } : undefined}>
          {char}
        </span>
      ))}
    </span>
  );
}

/** The reasoning effort as rising bars, one per level, lit up to `at`. */
function Gauge({ count, at }: { count: number; at: number }) {
  return (
    <span className="gauge" aria-hidden="true">
      {Array.from({ length: count }, (_, i) => (
        <span
          key={i}
          className={i <= at ? "gauge-bar is-lit" : "gauge-bar"}
          // From a quarter of the height for the lowest level to full for the highest.
          style={{ height: `${count > 1 ? 25 + (75 * i) / (count - 1) : 100}%` }}
        />
      ))}
    </span>
  );
}

function ModelButton({
  session,
  providers,
  open,
  onClick,
}: {
  session: SessionState | null;
  providers: Provider[];
  open: boolean;
  onClick: () => void;
}) {
  const model = session?.kind === "open" ? session.config.model : null;
  const current = model?.choices.find((choice) => choice.value === model.current);
  if (!model) {
    const label = session?.kind === "failed" ? "No session" : session?.kind === "open" ? "No model" : "Loading models";
    return (
      <span className="chip is-idle" data-tip={session?.kind === "failed" ? session.error : undefined} data-tip-align="start">
        {session?.kind === "opening" && <span className="spinner" />}
        {label}
      </span>
    );
  }
  return (
    <button
      className={open ? "chip is-open" : "chip"}
      type="button"
      data-tip="Model"
      data-tip-align="start"
      aria-haspopup="listbox"
      aria-expanded={open}
      data-menu-trigger="model"
      onClick={onClick}
    >
      {current && <ProviderLogo provider={current.provider} name={providerName(providers, current.provider)} />}
      <span className="chip-label">{current?.name ?? model.current}</span>
      <IconChevronDown className="chip-chevron" />
    </button>
  );
}

function EffortPicker({
  effort,
  open,
  onToggle,
  onClose,
}: {
  effort: NonNullable<SessionConfig["effort"]>;
  open: boolean;
  onToggle: () => void;
  onClose: () => void;
}) {
  return (
    <div className="picker">
      {open && (
        <Popover className="menu is-compact" trigger="effort" onClose={onClose}>
          <p className="menu-title">Reasoning effort</p>
          <ul className="menu-list" role="listbox" aria-label="Reasoning effort">
            {effort.levels.map((level) => (
              <li key={level} role="option" aria-selected={level === effort.current}>
                <button
                  className="menu-row"
                  type="button"
                  onClick={() => {
                    set({ configId: "thought_level", value: level });
                    onClose();
                  }}
                >
                  <Gauge count={effort.levels.length} at={effort.levels.indexOf(level)} />
                  <span className="menu-row-name">{effortLabel(level)}</span>
                  {level === effort.current && <IconCheck className="menu-check" />}
                </button>
              </li>
            ))}
          </ul>
        </Popover>
      )}
      <button
        className={open ? "chip is-open" : "chip"}
        type="button"
        data-tip="Reasoning effort"
        aria-haspopup="listbox"
        aria-expanded={open}
        data-menu-trigger="effort"
        onClick={onToggle}
      >
        <Gauge count={effort.levels.length} at={effort.levels.indexOf(effort.current)} />
        <span className="chip-label">{effortLabel(effort.current)}</span>
      </button>
    </div>
  );
}

function FastToggle({ on }: { on: boolean }) {
  return (
    <button
      className={on ? "chip is-on" : "chip"}
      type="button"
      data-tip={on ? "Fast mode on: priority speed and price" : "Fast mode off"}
      aria-label="Fast mode"
      aria-pressed={on}
      onClick={() => set({ configId: "fast_mode", value: !on })}
    >
      <IconBolt />
    </button>
  );
}

/**
 * The models with a search, each with its provider's mark and name; the
 * current provider's models first, then by provider.
 */
function ModelMenu({
  model,
  providers,
  onClose,
}: {
  model: NonNullable<SessionConfig["model"]>;
  providers: Provider[];
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const current = model.choices.find((choice) => choice.value === model.current);
  const rows = matching(model.choices, query, providers, current?.provider);
  const [active, setActive] = useState(() => Math.max(0, rows.findIndex((row) => row.choice.value === model.current)));
  const list = useRef<HTMLUListElement>(null);

  useEffect(() => {
    list.current?.querySelector(".menu-row.is-active")?.scrollIntoView({ block: "nearest" });
  }, [active, query]);

  const pick = (choice: ModelChoice | undefined) => {
    if (!choice) return;
    if (choice.value !== model.current) set({ configId: "model", value: choice.value });
    onClose();
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      setActive((index) => (rows.length ? (index + step + rows.length) % rows.length : 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      pick(rows[active]?.choice);
    }
  };

  return (
    <Popover className="menu" trigger="model" onClose={onClose}>
      <label className="search menu-search">
        <IconSearch />
        <input
          className="input"
          type="search"
          placeholder="Search models"
          aria-label="Search models"
          autoFocus
          spellCheck={false}
          value={query}
          onChange={(event) => {
            // A new search starts at its first result.
            setQuery(event.target.value);
            setActive(0);
          }}
          onKeyDown={onKeyDown}
        />
      </label>
      <ul className="menu-list" role="listbox" aria-label="Models" ref={list}>
        {rows.map(({ choice, provider }, at) => (
          <li key={choice.value} role="option" aria-selected={choice.value === model.current}>
            <button
              className={at === active ? "menu-row model-row is-active" : "menu-row model-row"}
              type="button"
              tabIndex={-1}
              onMouseMove={() => setActive(at)}
              onClick={() => pick(choice)}
            >
              <span className="model-badge">
                <ProviderLogo provider={choice.provider} name={provider.name} />
              </span>
              <span className="menu-row-main">
                <span className="model-title">
                  <span className="menu-row-name">{choice.name}</span>
                  <span className="model-provider">{provider.name}</span>
                </span>
                <Specs choice={choice} subscription={provider.subscription} />
              </span>
              {choice.value === model.current && <IconCheck className="menu-check" />}
            </button>
          </li>
        ))}
        {rows.length === 0 && <li className="menu-empty">No model matches “{query.trim()}”.</li>}
      </ul>
    </Popover>
  );
}

/**
 * What the model is, labelled: its context window, what it reads (text,
 * images; dim when not), its price per million tokens (none on a
 * subscription, where the plan pays, nor when unknown).
 */
function Specs({ choice, subscription }: { choice: ModelChoice; subscription: boolean }) {
  const text = choice.input.length === 0 || choice.input.includes("text");
  const image = choice.input.includes("image");
  const cost = choice.cost;
  const priced = !subscription && cost && (cost.input > 0 || cost.output > 0);
  return (
    <span className="specs">
      {choice.contextWindow > 0 && (
        <span className="spec">
          <span className="spec-label">Context</span>
          <span className="spec-value">{tokens(choice.contextWindow)}</span>
        </span>
      )}
      <span className="spec" aria-label={image ? "Input: text and images" : "Input: text only"}>
        <span className="spec-label">Input</span>
        <IconText className={text ? undefined : "is-off"} />
        <IconImage className={image ? undefined : "is-off"} />
      </span>
      {priced && (
        <span className="spec">
          <span className="spec-label">Price</span>
          <span className="spec-value">
            {dollars(cost.input)}/M in · {dollars(cost.output)}/M out
          </span>
        </span>
      )}
    </span>
  );
}

/** A menu above the composer; closes on Escape or a press outside it and its trigger. */
function Popover({
  className,
  trigger,
  onClose,
  children,
}: {
  className: string;
  trigger: Menu;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useDismiss(ref, trigger, onClose);
  return (
    <div className={className} ref={ref}>
      {children}
    </div>
  );
}

function useDismiss(ref: RefObject<HTMLElement | null>, trigger: Menu, onClose: () => void) {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Element | null;
      if (ref.current?.contains(target)) return;
      if (target?.closest(`[data-menu-trigger="${trigger}"]`)) return;
      close.current();
    };
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") close.current();
    };
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [ref, trigger]);
}

type Row = { choice: ModelChoice; provider: { name: string; subscription: boolean } };

/** The models the search matches, the `first` provider's first, then by provider name. */
function matching(choices: ModelChoice[], query: string, providers: Provider[], first: string | undefined): Row[] {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const rows: Row[] = [];
  for (const choice of choices) {
    const name = providerName(providers, choice.provider);
    const haystack = `${choice.name} ${choice.id} ${name}`.toLowerCase();
    if (!words.every((word) => haystack.includes(word))) continue;
    rows.push({ choice, provider: { name, subscription: subscription(providers, choice.provider) } });
  }
  // Stable: each provider's models keep the agent's order.
  return rows.sort((a, b) => {
    if (a.choice.provider === b.choice.provider) return 0;
    if (a.choice.provider === first) return -1;
    if (b.choice.provider === first) return 1;
    return a.provider.name.localeCompare(b.provider.name) || a.choice.provider.localeCompare(b.choice.provider);
  });
}

/**
 * The connected row of the provider; Anthropic and xAI have a subscription
 * row and an API key row under the same id, the subscription first.
 */
function connectedRow(providers: Provider[], id: string): Provider | undefined {
  return providers.find((p) => p.id === id && p.connected) ?? providers.find((p) => p.id === id);
}

function providerName(providers: Provider[], id: string): string {
  return connectedRow(providers, id)?.name ?? id;
}

/** A subscription pays for the models: no price per token to show. */
function subscription(providers: Provider[], id: string): boolean {
  const row = connectedRow(providers, id);
  return row?.connected === true && row.method === "subscription";
}

/** 1000000 → `1M`, 400000 → `400K`, 1048576 → `1M`. */
function tokens(count: number): string {
  if (count >= 1_000_000) return `${trim(count / 1_000_000, 1)}M`;
  if (count >= 1_000) return `${Math.round(count / 1_000)}K`;
  return String(count);
}

/** 4 → `$4`, 1.25 → `$1.25`, 0.075 → `$0.075`. */
function dollars(amount: number): string {
  return `$${trim(amount, amount < 1 ? 3 : 2)}`;
}

function trim(value: number, digits: number): string {
  return String(Number(value.toFixed(digits)));
}
