import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import type { ConfigChange, Provider, SessionConfig, SessionState } from "../src/protocol";
import { IconBolt, IconCheck, IconChevronDown, IconSend } from "./icons";
import { ProviderLogo } from "./logos";
import { ModelList, providerName } from "./models";
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

/** The model picker, above the composer. */
function ModelMenu({
  model,
  providers,
  onClose,
}: {
  model: NonNullable<SessionConfig["model"]>;
  providers: Provider[];
  onClose: () => void;
}) {
  return (
    <Popover className="menu" trigger="model" onClose={onClose}>
      <ModelList
        choices={model.choices}
        selected={model.current}
        keyOf={(choice) => choice.value}
        providers={providers}
        onPick={(choice) => {
          if (choice && choice.value !== model.current) set({ configId: "model", value: choice.value });
          onClose();
        }}
      />
    </Popover>
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
