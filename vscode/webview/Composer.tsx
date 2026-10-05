import {
  useEffect,
  useRef,
  useState,
  type ClipboardEvent,
  type DragEvent,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import type {
  Attachment,
  ChatCommand,
  ChatState,
  ConfigChange,
  Provider,
  QueuedMessage,
  SessionConfig,
  SessionState,
} from "../src/protocol";
import { attach, IMAGE_TYPES } from "./attachments";
import { IconBolt, IconCheck, IconChevronDown, IconClip, IconCross, IconSend, IconStop } from "./icons";
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

type Menu = "model" | "effort" | "slash";

/** What is being written: kept above the box, so it survives a visit to the settings. */
export type Draft = { text: string; images: Attachment[] };

/** The image settings, as the agent applies them. */
export type ImageSettings = { block: boolean; resize: boolean; imageModel: string | null };

/** Images a message carries at most. */
const MAX_IMAGES = 10;

const GROUPS: [string, ChatCommand["kind"][]][] = [
  ["Commands", ["panel", "command"]],
  ["Prompts", ["prompt"]],
  ["Skills", ["skill"]],
];

/**
 * The message box at the bottom of home: what is being written and the
 * images it carries, the messages waiting for the running turn, and the
 * session's pickers (the model, its reasoning effort, its fast mode).
 * Enter sends; while a turn runs the message waits, and stop ends the turn.
 * A `/` at the start opens the commands, prompts and skills.
 */
export function Composer({
  session,
  providers,
  chat,
  images,
  draft,
  onDraft,
}: {
  session: SessionState | null;
  providers: Provider[];
  chat: ChatState;
  images: ImageSettings;
  draft: Draft;
  onDraft: (update: (draft: Draft) => Draft) => void;
}) {
  const [menu, setMenu] = useState<Menu | null>(null);
  /** Bumped when the empty box takes focus: the placeholder streams in again. */
  const [stream, setStream] = useState(0);
  const [notes, setNotes] = useState<string[]>([]);
  const [dragging, setDragging] = useState(false);
  /** The text the `/` menu was dismissed on: it stays shut until the text changes. */
  const [dismissed, setDismissed] = useState<string | null>(null);
  const [highlight, setHighlight] = useState(0);
  const input = useRef<HTMLTextAreaElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const config = session?.kind === "open" ? session.config : null;
  const toggle = (next: Menu) => setMenu((open) => (open === next ? null : next));
  const close = () => setMenu(null);
  const hasContent = draft.text.trim() !== "" || draft.images.length > 0;

  // A menu whose picker left (a model switch drops the effort) closes.
  useEffect(() => {
    if (menu === "model" && !config?.model) setMenu(null);
    if (menu === "effort" && !config?.effort) setMenu(null);
  }, [menu, config]);

  const commands = slashCommands(draft.text, chat.commands);
  const slashOpen = commands !== null && dismissed !== draft.text && menu === null;
  useEffect(() => setHighlight(0), [draft.text]);

  const setText = (text: string) => onDraft((current) => ({ ...current, text }));

  const addFiles = (files: File[]) => {
    const found = files.filter((file) => IMAGE_TYPES.includes(file.type) || file.type.startsWith("image/"));
    if (found.length === 0) return;
    if (images.block) {
      setNotes(["Images are blocked: turn off “Block images” in the settings to send them."]);
      return;
    }
    const room = MAX_IMAGES - draft.images.length;
    setNotes(found.length > room ? [`A message carries ${MAX_IMAGES} images at most.`] : []);
    for (const file of found.slice(0, Math.max(0, room))) {
      attach(file, images.resize).then(
        (attachment) =>
          onDraft((current) =>
            current.images.length >= MAX_IMAGES ? current : { ...current, images: [...current.images, attachment] },
          ),
        (error: unknown) => setNotes((current) => [...current, error instanceof Error ? error.message : String(error)]),
      );
    }
  };

  const submit = () => {
    if (!hasContent) return;
    post({ type: "send", text: draft.text.trim(), images: draft.images });
    onDraft(() => ({ text: "", images: [] }));
    setNotes([]);
  };

  const pick = (command: ChatCommand) => {
    setText(`/${command.name} `);
    input.current?.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.nativeEvent.isComposing) return;
    if (slashOpen && commands.length > 0) {
      const move = { ArrowDown: 1, ArrowUp: -1 }[event.key];
      if (move) {
        event.preventDefault();
        setHighlight((at) => (at + move + commands.length) % commands.length);
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        pick(commands[Math.min(highlight, commands.length - 1)]);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        setDismissed(draft.text);
        return;
      }
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      submit();
    }
  };

  const onPaste = (event: ClipboardEvent) => {
    const files = [...event.clipboardData.files];
    if (files.some((file) => file.type.startsWith("image/"))) {
      event.preventDefault();
      addFiles(files);
    }
  };

  const onDrop = (event: DragEvent) => {
    setDragging(false);
    if (event.dataTransfer.files.length === 0) return;
    event.preventDefault();
    addFiles([...event.dataTransfer.files]);
  };

  const current = config?.model?.choices.find((choice) => choice.value === config.model?.current);
  // A model whose inputs are known, without images among them, and no image model to read them.
  const unreadable =
    draft.images.length > 0 && !images.imageModel && !!current?.input.length && !current.input.includes("image");

  return (
    <>
      {chat.queue.length > 0 && <Queue queue={chat.queue} />}
      <div
        className={dragging ? "composer-shell is-dragging" : "composer-shell"}
        onDragOver={(event) => {
          if (!event.dataTransfer.types.includes("Files")) return;
          event.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={onDrop}
      >
        <div className="composer-glow" aria-hidden="true" />
        <div className="composer">
          {menu === "model" && config?.model && (
            <ModelMenu model={config.model} providers={providers} onClose={close} />
          )}
          {slashOpen && (
            <SlashMenu
              commands={commands}
              highlight={highlight}
              onHighlight={setHighlight}
              onPick={pick}
              onClose={() => setDismissed(draft.text)}
            />
          )}
          {draft.images.length > 0 && (
            <Attachments
              images={draft.images}
              onRemove={(at) => onDraft((d) => ({ ...d, images: d.images.filter((_, i) => i !== at) }))}
            />
          )}
          <div className="composer-field">
            <textarea
              ref={input}
              className="composer-input"
              rows={3}
              aria-label={PLACEHOLDER}
              spellCheck={false}
              value={draft.text}
              onChange={(event) => setText(event.target.value)}
              onFocus={() => draft.text === "" && setStream((n) => n + 1)}
              onKeyDown={onKeyDown}
              onPaste={onPaste}
            />
            {draft.text === "" && <Placeholder key={stream} streaming={stream > 0} />}
          </div>
          {notes.map((note, at) => (
            <p className="composer-note" key={at}>
              {note}
            </p>
          ))}
          {unreadable && (
            <p className="composer-note">
              {current.name} doesn't read images: pick an image model in the settings, or a model that reads them.
            </p>
          )}
          {session?.kind === "open" && session.error && <p className="composer-error">{session.error}</p>}
          <div className="composer-bar">
            <input
              ref={picker}
              type="file"
              accept={IMAGE_TYPES.join(",")}
              multiple
              hidden
              onChange={(event) => {
                addFiles([...(event.target.files ?? [])]);
                event.target.value = "";
              }}
            />
            <button
              className="chip"
              type="button"
              aria-label="Attach images"
              data-tip={images.block ? "Images are blocked in the settings" : "Attach images"}
              data-tip-align="start"
              disabled={images.block}
              onClick={() => picker.current?.click()}
            >
              <IconClip />
            </button>
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
            {chat.busy && (
              <button
                className="send is-stop"
                type="button"
                aria-label="Stop"
                data-tip="Stop"
                data-tip-align="end"
                onClick={() => post({ type: "cancel" })}
              >
                <IconStop />
              </button>
            )}
            {(!chat.busy || hasContent) && (
              <button
                className="send"
                type="button"
                aria-label="Send"
                data-tip={chat.busy ? "Send when the agent is done" : "Send"}
                data-tip-align="end"
                disabled={!hasContent}
                onClick={submit}
              >
                <IconSend />
              </button>
            )}
          </div>
        </div>
      </div>
    </>
  );
}

/**
 * The commands matching a `/` at the start of the text, before any space;
 * `null` when the text is not one. Names starting with what is typed come first.
 */
function slashCommands(text: string, commands: ChatCommand[]): ChatCommand[] | null {
  const match = /^\/(\S*)$/.exec(text);
  if (!match) return null;
  const query = match[1].toLowerCase();
  const found = commands.filter((command) => command.name.toLowerCase().includes(query));
  const starts = (command: ChatCommand) => (command.name.toLowerCase().startsWith(query) ? 0 : 1);
  return GROUPS.flatMap(([, kinds]) =>
    found.filter((command) => kinds.includes(command.kind)).sort((a, b) => starts(a) - starts(b)),
  );
}

/** The commands, prompts and skills a `/` opens; the highlighted one is picked with Enter or Tab. */
function SlashMenu({
  commands,
  highlight,
  onHighlight,
  onPick,
  onClose,
}: {
  commands: ChatCommand[];
  highlight: number;
  onHighlight: (at: number) => void;
  onPick: (command: ChatCommand) => void;
  onClose: () => void;
}) {
  const list = useRef<HTMLUListElement>(null);
  useEffect(() => {
    list.current?.querySelector(`[data-at="${highlight}"]`)?.scrollIntoView({ block: "nearest" });
  }, [highlight]);
  if (commands.length === 0) {
    return (
      <Popover className="menu" trigger="slash" onClose={onClose}>
        <p className="menu-empty">No command, prompt or skill by that name.</p>
      </Popover>
    );
  }
  let at = 0;
  return (
    <Popover className="menu" trigger="slash" onClose={onClose}>
      <ul className="menu-list slash-list" role="listbox" aria-label="Commands" ref={list}>
        {GROUPS.map(([title, kinds]) => {
          const rows = commands.filter((command) => kinds.includes(command.kind));
          if (rows.length === 0) return null;
          return (
            <li key={title} role="presentation">
              <p className="menu-title">{title}</p>
              <ul className="menu-list" role="presentation">
                {rows.map((command) => {
                  const index = at++;
                  return (
                    <li key={command.name} role="option" aria-selected={index === highlight} data-at={index}>
                      <button
                        className={index === highlight ? "menu-row slash-row is-active" : "menu-row slash-row"}
                        type="button"
                        onMouseEnter={() => onHighlight(index)}
                        onClick={() => onPick(command)}
                      >
                        <span className="slash-name">
                          /{command.name}
                          {command.hint && <span className="slash-hint">{command.hint}</span>}
                        </span>
                        <span className="slash-description">{command.description}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            </li>
          );
        })}
      </ul>
    </Popover>
  );
}

/** The images the message carries, each with its remove button. */
function Attachments({ images, onRemove }: { images: Attachment[]; onRemove: (at: number) => void }) {
  return (
    <div className="attachments">
      {images.map((image, at) => (
        <div className="attachment" key={at}>
          <img src={image.thumb} alt={image.name} title={image.name} />
          <button
            className="attachment-remove"
            type="button"
            aria-label={`Remove ${image.name}`}
            onClick={() => onRemove(at)}
          >
            <IconCross />
          </button>
        </div>
      ))}
    </div>
  );
}

/** The messages waiting for the running turn, in the order they go. */
function Queue({ queue }: { queue: QueuedMessage[] }) {
  return (
    <ul className="queue" aria-label="Waiting messages">
      {queue.map((message) => (
        <li className="queued" key={message.id}>
          <span className="queued-label">Next</span>
          <span className="queued-text">
            {message.text || `${message.images.length} image${message.images.length > 1 ? "s" : ""}`}
          </span>
          <button
            className="queued-remove"
            type="button"
            aria-label="Remove"
            data-tip="Remove"
            data-tip-align="end"
            onClick={() => post({ type: "unqueue", id: message.id })}
          >
            <IconCross />
          </button>
        </li>
      ))}
    </ul>
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
