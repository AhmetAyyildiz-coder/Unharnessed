import { memo, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import type { ChatEntry, ChatPatch, ChatState, Thumb, TurnItem } from "../src/protocol";
import { IconArrowDown, IconCell, IconCheck, IconChevron, IconCross, IconRestart } from "./icons";
import { ThinkingOrb } from "thinking-orbs";
import { Markdown } from "./Markdown";

type Turn = Extract<ChatEntry, { kind: "turn" }>;
type Tool = Extract<TurnItem, { kind: "tool" }>;

/** How close to the end the view must be to follow what streams in. */
const FOLLOW_PX = 48;
/** The reasoning's window: it grows to this, then scrolls; its edges fade over this (two lines). */
const THOUGHT_MAX_PX = 80;
const THOUGHT_FADE_PX = 40;
/** How long reasoning that just ended stays open before it folds. */
const FOLD_BEAT_MS = 360;
/** How long the answer may pause before the turn says again that it works. */
const QUIET_MS = 900;
/** How long an orb takes to leave once its state is over (styles.css, .orb). */
const ORB_OUT_MS = 320;

const WAKES: Record<NonNullable<Turn["wake"]>, string> = {
  bash: "a background command ended",
  quota: "the usage limit reset",
  goal: "the goal's pause ended",
};

export const EMPTY_CHAT: ChatState = { entries: [], busy: false, queue: [], commands: [] };

/** What changed, onto the conversation; what did not change keeps its identity (and its render). */
export function applyPatch(chat: ChatState, patch: ChatPatch): ChatState {
  let entries = chat.entries;
  if (patch.heads.length > 0 || patch.items.length > 0) {
    entries = [...entries];
    const index = new Map(entries.map((entry, at) => [entry.id, at]));
    for (const head of patch.heads) {
      const at = index.get(head.id);
      const before = at === undefined ? undefined : entries[at];
      const entry: ChatEntry =
        head.kind === "turn" ? { ...head, items: before?.kind === "turn" ? before.items : [] } : head;
      if (at === undefined) index.set(head.id, entries.push(entry) - 1);
      else entries[at] = entry;
    }
    const byEntry = new Map<number, [number, TurnItem][]>();
    for (const [id, at, item] of patch.items) byEntry.set(id, [...(byEntry.get(id) ?? []), [at, item]]);
    for (const [id, changes] of byEntry) {
      const at = index.get(id);
      const entry = at === undefined ? undefined : entries[at];
      if (at === undefined || entry?.kind !== "turn") continue;
      const items = [...entry.items];
      for (const [itemAt, item] of changes) items[itemAt] = item;
      entries[at] = { ...entry, items };
    }
  }
  return { entries, busy: patch.busy, queue: patch.queue, commands: patch.commands ?? chat.commands };
}

/** The conversation; it follows what streams in while it is scrolled to the end. */
export function Transcript({ chat, children }: { chat: ChatState; children?: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const [away, setAway] = useState(false);
  const last = chat.entries.at(-1);

  // A message the user just sent brings the view back to the end.
  const sent = last?.kind === "turn" && last.prompt ? last.id : undefined;
  useLayoutEffect(() => {
    if (sent !== undefined) follow.current = true;
  }, [sent]);

  // What grows between two patches (the reasoning's window opening, a fold
  // closing) is followed too: each entry is watched for its size.
  const sizes = useRef<ResizeObserver>(undefined);
  useLayoutEffect(() => {
    const view = ref.current;
    if (!view) return;
    if (follow.current) view.scrollTop = view.scrollHeight;
    sizes.current ??= new ResizeObserver(() => {
      if (follow.current && ref.current) ref.current.scrollTop = ref.current.scrollHeight;
    });
    for (const entry of view.children) sizes.current.observe(entry);
  });
  useEffect(() => () => sizes.current?.disconnect(), []);

  const onScroll = () => {
    const view = ref.current;
    if (!view) return;
    follow.current = view.scrollHeight - view.scrollTop - view.clientHeight < FOLLOW_PX;
    setAway(!follow.current);
  };

  const toEnd = () => {
    follow.current = true;
    setAway(false);
    if (ref.current) ref.current.scrollTop = ref.current.scrollHeight;
  };

  return (
    <div className="transcript-shell">
      <div className="transcript" ref={ref} onScroll={onScroll}>
        {children}
        {chat.entries.map((entry) =>
          entry.kind === "restart" ? <Restart key={entry.id} /> : <TurnView key={entry.id} turn={entry} />,
        )}
      </div>
      {away && (
        <button className="icon-button to-end" type="button" aria-label="Latest" data-tip="Latest" onClick={toEnd}>
          <IconArrowDown />
        </button>
      )}
    </div>
  );
}

function Restart() {
  return (
    <div className="restart-line">
      <IconRestart />
      The agent restarted: it no longer remembers the messages above.
    </div>
  );
}

const TurnView = memo(function TurnView({ turn }: { turn: Turn }) {
  const running = turn.state === "running";
  // Reasoning, a running cell and the answer as it streams show their own
  // life; otherwise (a pause: the model writing a cell, say) the turn says it works.
  const last = turn.items.at(-1);
  const writing = useWriting(running && last?.kind === "text" ? last.text : undefined);
  const busy =
    writing ||
    (last?.kind === "thought" && last.ms === undefined) ||
    (last?.kind === "tool" && last.status === "running");
  const working = running && !busy;
  return (
    <section className="turn">
      {turn.prompt && <UserMessage text={turn.prompt.text} images={turn.prompt.images} />}
      {!turn.prompt && turn.wake && <p className="turn-wake">The agent went on by itself: {WAKES[turn.wake]}.</p>}
      {turn.items.map((item, at) => (
        <Item key={at} item={item} running={running} streaming={running && at === turn.items.length - 1} />
      ))}
      {working && (
        <p className="working-line">
          <Orb on state="breathing" />
          <span className="working">Working</span>
        </p>
      )}
      {turn.state === "cancelled" && <p className="turn-end">Stopped</p>}
      {turn.state === "failed" && turn.error && <p className="error turn-error">{turn.error}</p>}
    </section>
  );
});

function UserMessage({ text, images }: { text: string; images: Thumb[] }) {
  return (
    <div className="user-message">
      {images.length > 0 && (
        <div className="user-images">
          {images.map((image, at) => (
            <img key={at} src={image.thumb} alt={image.name} title={image.name} />
          ))}
        </div>
      )}
      {text && <p className="user-text">{text}</p>}
    </div>
  );
}

/** Whether `text` (the answer under way) changed within the last QUIET_MS. */
function useWriting(text: string | undefined): boolean {
  const [quiet, setQuiet] = useState(true);
  // Before the paint, so "Working" never flashes as a chunk lands.
  useLayoutEffect(() => {
    if (text === undefined) return;
    setQuiet(false);
    const timer = setTimeout(() => setQuiet(true), QUIET_MS);
    return () => clearTimeout(timer);
  }, [text]);
  return text !== undefined && !quiet;
}

const Item = memo(function Item({ item, running, streaming }: { item: TurnItem; running: boolean; streaming: boolean }) {
  switch (item.kind) {
    case "text":
      return <Markdown text={item.text} streaming={streaming} />;
    case "thought":
      return <Thought thought={item} live={running && item.ms === undefined} />;
    case "tool":
      return <ToolCall tool={item} />;
    case "notice":
      return <p className={`notice is-${item.tone}`}>{item.text}</p>;
  }
});

/**
 * The model's reasoning. While it thinks, its words stream open in a window
 * that grows to THOUGHT_MAX_PX, then keeps the newest lines in view under a
 * fade; once it is over it folds into "Thought for 6s", which opens again
 * as a window to scroll.
 */
function Thought({ thought, live }: { thought: Extract<TurnItem, { kind: "thought" }>; live: boolean }) {
  const text = thought.text.trim();
  const [open, setOpen] = useState(false);
  // The reasoning that just ended stays open a beat before it folds.
  const [held, setHeld] = useState(false);
  const wasLive = useRef(live);
  useLayoutEffect(() => {
    const ended = wasLive.current && !live;
    wasLive.current = live;
    if (!ended) return;
    setHeld(true);
    const timer = setTimeout(() => setHeld(false), FOLD_BEAT_MS);
    return () => clearTimeout(timer);
  }, [live]);

  // The words' height, to size the window and keep the newest in view.
  const stream = useRef<HTMLDivElement>(null);
  const view = useRef<HTMLDivElement>(null);
  const [height, setHeight] = useState(0);
  const hasText = text !== "";
  useLayoutEffect(() => {
    const element = stream.current;
    if (!element) return;
    const measure = () => setHeight(element.offsetHeight);
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    measure();
    return () => observer.disconnect();
  }, [hasText]);
  const [fade, setFade] = useState({ top: false, bottom: true });

  const thinking = live || held;
  const expanded = hasText && (thinking || open);
  const scrolls = !thinking && open;
  const capped = height > THOUGHT_MAX_PX;
  // While it streams the newest words sit at the bottom, the oldest fading out at the top.
  const shift = !scrolls && capped ? THOUGHT_MAX_PX - height : 0;
  const top = scrolls ? fade.top : capped;
  const bottom = scrolls && fade.bottom;
  const mask = capped
    ? `linear-gradient(to bottom, transparent 0, #000 ${top ? THOUGHT_FADE_PX : 0}px, #000 calc(100% - ${bottom ? THOUGHT_FADE_PX : 0}px), transparent 100%)`
    : undefined;

  const toggle = () => {
    if (!open) {
      setFade({ top: false, bottom: true });
      if (view.current) view.current.scrollTop = 0;
    }
    setOpen(!open);
  };
  const onScroll = () => {
    const element = view.current;
    if (!element) return;
    setFade({
      top: element.scrollTop > 1,
      bottom: element.scrollTop + element.clientHeight < element.scrollHeight - 1,
    });
  };

  const opens = !thinking && hasText;
  return (
    <div className="thought">
      <button
        className={opens ? "thought-head" : "thought-head is-still"}
        type="button"
        data-no-press
        aria-expanded={expanded}
        onClick={opens ? toggle : undefined}
      >
        <Orb on={thinking} state="composing" />
        {thinking ? (
          <span className="thought-label">
            <span className="thought-verb is-live">Thinking</span>
            {live ? <Seconds since={thought.started} /> : thought.ms !== undefined && ` ${took(thought.ms)}`}
          </span>
        ) : (
          <span className="thought-label">
            <span className="thought-verb">Thought</span>
            {thought.ms !== undefined && ` for ${took(thought.ms)}`}
          </span>
        )}
        {opens && <IconChevron className="thought-chevron" />}
      </button>
      {hasText && (
        <div className={expanded ? "thought-fold" : "thought-fold is-folded"}>
          <div className="thought-inner">
            <div
              ref={view}
              className={scrolls ? "thought-view is-scroll" : "thought-view"}
              style={{ height: Math.min(height, THOUGHT_MAX_PX), WebkitMaskImage: mask, maskImage: mask }}
              onScroll={scrolls ? onScroll : undefined}
            >
              <div ref={stream} className="thought-stream" style={{ transform: `translateY(${shift}px)` }}>
                <Markdown text={text} streaming={live} />
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * The orb beside "Thinking" and "Working" (thinking-orbs), in the cyan of
 * live things, glowing (styles.css, .orb): in while it is `on`, out softly
 * once it is not.
 */
function Orb({ on, state }: { on: boolean; state: "composing" | "breathing" }) {
  const [shown, setShown] = useState(on);
  const ink = useInk();
  useEffect(() => {
    if (on) {
      setShown(true);
      return;
    }
    const timer = setTimeout(() => setShown(false), ORB_OUT_MS);
    return () => clearTimeout(timer);
  }, [on]);
  if (!shown) return null;
  return (
    <span className={on ? "orb" : "orb is-leaving"} aria-hidden="true">
      <ThinkingOrb state={state} size={20} theme={ink.light ? "light" : "dark"} color={ink.accent} />
    </span>
  );
}

/** The panel's live colour (the cyan) and whether VS Code's theme is light, following theme changes. */
function useInk(): { accent: string; light: boolean } {
  const [ink, setInk] = useState(readInk);
  useEffect(() => {
    const observer = new MutationObserver(() => setInk(readInk()));
    observer.observe(document.body, { attributes: true, attributeFilter: ["class"] });
    return () => observer.disconnect();
  }, []);
  return ink;
}

function readInk(): { accent: string; light: boolean } {
  const rgb = getComputedStyle(document.body).getPropertyValue("--uh-accent-rgb").trim().split(/\s+/);
  const classes = document.body.classList;
  return {
    accent: `rgb(${rgb.join(", ")})`,
    light: classes.contains("vscode-light") || classes.contains("vscode-high-contrast-light"),
  };
}

/** ` 3s` after `since`, counting each second; nothing in the first. */
function Seconds({ since }: { since: number }) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    // The next whole second.
    const timer = setTimeout(() => setNow(Date.now()), 1000 - ((Date.now() - since) % 1000));
    return () => clearTimeout(timer);
  }, [now, since]);
  return now - since < 1000 ? null : ` ${took(now - since)}`;
}

/** `6s`, `1m 12s`; whole seconds, as the count while it runs showed them. */
function took(ms: number): string {
  const seconds = Math.max(1, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const rest = seconds % 60;
  return `${Math.floor(seconds / 60)}m${rest ? ` ${rest}s` : ""}`;
}

/** A cell the agent runs: its first line, then on demand the code and what it printed. */
function ToolCall({ tool }: { tool: Tool }) {
  const [open, setOpen] = useState(false);
  const firstLine = tool.input.split("\n").find((line) => line.trim()) ?? "";
  const extras = [
    tool.images ? `${tool.images} image${tool.images > 1 ? "s" : ""} loaded` : "",
    tool.diffs ? `${tool.diffs} diff${tool.diffs > 1 ? "s" : ""}` : "",
  ].filter(Boolean);
  return (
    <div className={`tool is-${tool.status}${open ? " is-open" : ""}`}>
      <button className="fold tool-head" type="button" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span className="tool-status">
          {tool.status === "running" ? (
            <span className="spinner" />
          ) : tool.status === "failed" ? (
            <IconCross />
          ) : (
            <IconCheck />
          )}
        </span>
        <IconCell className="tool-icon" />
        <span className="tool-title">{tool.title === "Python cell" ? "Python" : tool.title}</span>
        <span className="fold-preview is-mono">{firstLine}</span>
        <IconChevron className="fold-chevron" />
      </button>
      {open && (
        <div className="tool-body">
          <pre className="tool-code">{tool.input}</pre>
          {tool.output && <pre className="tool-output">{tool.output}</pre>}
          {extras.length > 0 && <p className="tool-extras">{extras.join(" · ")}</p>}
        </div>
      )}
    </div>
  );
}
