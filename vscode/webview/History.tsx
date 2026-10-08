import { useEffect, useState } from "react";
import type { HistoryState, SavedChat } from "../src/protocol";
import { IconCheck, IconSearch, IconTrash } from "./icons";
import { Popover } from "./Popover";
import { post } from "./vscode";

/** From this many conversations on, a search field filters them. */
const SEARCH_FROM = 8;
const DAY_MS = 86_400_000;

const TIME = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit" });
const WEEKDAY = new Intl.DateTimeFormat("en-GB", { weekday: "short" });
const DATE = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short" });

/**
 * The conversations of the panel's folder, the newest first, in a menu under
 * the top bar's clock: one opens in the panel (the agent restarts on it), the
 * one in the panel has the check. Delete comes with a row's hover and asks
 * once more, in place.
 */
export function HistoryMenu({ history, onClose }: { history: HistoryState; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [confirming, setConfirming] = useState<string | null>(null);
  // Read again each time the menu opens.
  useEffect(() => post({ type: "listChats" }), []);

  const { chats } = history;
  const words = query.trim().toLowerCase();
  const shown = (chats ?? []).filter((chat) => !words || title(chat).toLowerCase().includes(words));

  return (
    <Popover className="menu history-menu" trigger="history" onClose={onClose}>
      {(chats?.length ?? 0) >= SEARCH_FROM ? (
        <label className="search menu-search">
          <IconSearch />
          <input
            className="input"
            type="search"
            placeholder="Search conversations"
            aria-label="Search conversations"
            autoFocus
            spellCheck={false}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
      ) : (
        <p className="menu-title">History</p>
      )}
      <ul className="menu-list" aria-label="Conversations">
        {shown.map((chat) => (
          <li key={chat.id} className="history-row" onMouseLeave={() => setConfirming(null)}>
            <button
              className="menu-row"
              type="button"
              onClick={() => {
                if (!chat.current) post({ type: "openChat", id: chat.id });
                onClose();
              }}
            >
              <span className={chat.title ? "menu-row-name" : "menu-row-name is-untitled"}>{title(chat)}</span>
              {chat.current ? <IconCheck className="menu-check" /> : <span className="history-when">{when(chat.updated)}</span>}
            </button>
            {!chat.current &&
              (confirming === chat.id ? (
                <button
                  className="history-delete is-confirm"
                  type="button"
                  autoFocus
                  onClick={() => {
                    setConfirming(null);
                    post({ type: "deleteChat", id: chat.id });
                  }}
                >
                  Delete
                </button>
              ) : (
                <button
                  className="history-delete"
                  type="button"
                  aria-label={`Delete ${title(chat)}`}
                  onClick={() => setConfirming(chat.id)}
                >
                  <IconTrash />
                </button>
              ))}
          </li>
        ))}
      </ul>
      {history.error && <p className="menu-empty is-error">{history.error}</p>}
      {!history.error && chats === null && <p className="menu-empty">Reading…</p>}
      {chats?.length === 0 && <p className="menu-empty">No conversation yet in this folder.</p>}
      {chats && chats.length > 0 && shown.length === 0 && (
        <p className="menu-empty">No conversation matches “{query.trim()}”.</p>
      )}
    </Popover>
  );
}

function title(chat: SavedChat): string {
  return chat.title ?? "Untitled";
}

/** `14:32` today, `Yesterday`, `Mon` this week, `3 Oct` before. */
function when(updated: number): string {
  if (!updated) return "";
  const date = new Date(updated);
  const today = new Date().setHours(0, 0, 0, 0);
  if (updated >= today) return TIME.format(date);
  if (updated >= today - DAY_MS) return "Yesterday";
  if (updated >= today - 6 * DAY_MS) return WEEKDAY.format(date);
  return DATE.format(date);
}
