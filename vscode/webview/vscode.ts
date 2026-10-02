import type { HostMessage, WebviewMessage } from "../src/protocol";

declare function acquireVsCodeApi(): { postMessage(message: unknown): void };

const api = acquireVsCodeApi();

export function post(message: WebviewMessage): void {
  api.postMessage(message);
}

export function onHostMessage(handler: (message: HostMessage) => void): () => void {
  const listener = (event: MessageEvent<HostMessage>) => handler(event.data);
  window.addEventListener("message", listener);
  return () => window.removeEventListener("message", listener);
}
