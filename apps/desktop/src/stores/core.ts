// Process-wide Core state: connection status and the live event feed.
// Started once from main.tsx; components read it through hooks.

import { useEffect } from "react";

import { coreRequest, getConnection, onConnection, onCoreEvent, onLive, runtimeAvailable, toCoreError } from "../lib/core";
import { mergeEvents } from "../lib/format";
import type { ConnectionState, EventEnvelope, LiveOutput } from "../types/protocol";
import { createStore, useStore } from "./store";

const EVENT_BUFFER = 500;
const INITIAL_EVENT_PAGE = 200;

export const connectionStore = createStore<ConnectionState>({ state: "connecting", attempt: 0 });

export interface EventFeed {
  events: EventEnvelope[];
  /** History has been loaded for the current connection. */
  loaded: boolean;
  error: string | null;
}

export const eventFeedStore = createStore<EventFeed>({ events: [], loaded: false, error: null });

/** Ephemeral in-progress assistant text per session (never journaled). */
export interface LiveState {
  text: string;
  thinking: boolean;
  seq: number;
  /** Some output was dropped (slow client or reconnect). */
  gap: boolean;
}

export const liveStore = createStore<Record<string, LiveState>>({});

export function applyLive(state: Record<string, LiveState>, o: LiveOutput): Record<string, LiveState> {
  if (o.kind === "gap") {
    // A gap without a session (all-session subscription) marks every buffer.
    const next: Record<string, LiveState> = {};
    for (const [id, s] of Object.entries(state)) next[id] = { ...s, gap: true };
    return next;
  }
  const current = state[o.session_id] ?? { text: "", thinking: false, seq: 0, gap: false };
  if (o.seq <= current.seq) return state;
  let next: LiveState;
  switch (o.kind) {
    case "message_start":
      next = { text: "", thinking: false, seq: o.seq, gap: false };
      break;
    case "text":
      next = { ...current, text: current.text + o.text, thinking: false, seq: o.seq };
      break;
    case "thinking":
      next = { ...current, thinking: true, seq: o.seq };
      break;
    case "message_stop":
      next = { ...current, thinking: false, seq: o.seq };
      break;
  }
  return { ...state, [o.session_id]: next };
}

type EventListener = (event: EventEnvelope) => void;
const eventListeners = new Set<EventListener>();

/** Subscribes to live Core events (for refreshing views). */
export function useCoreEvents(listener: EventListener): void {
  useEffect(() => {
    eventListeners.add(listener);
    return () => {
      eventListeners.delete(listener);
    };
  }, [listener]);
}

export const useConnection = (): ConnectionState => useStore(connectionStore);
export const useEventFeed = (): EventFeed => useStore(eventFeedStore);

/** Identifies the current Core connection; changes on every (re)connect. */
export function connectionKey(state: ConnectionState): string | null {
  return state.state === "connected" ? `${state.core.instance_id}` : null;
}

async function loadHistory(): Promise<void> {
  try {
    const { events } = await coreRequest("events.list", { limit: INITIAL_EVENT_PAGE });
    eventFeedStore.set((feed) => ({ events: mergeEvents(feed.events, events, EVENT_BUFFER), loaded: true, error: null }));
  } catch (error) {
    eventFeedStore.set((feed) => ({ ...feed, error: toCoreError(error).message }));
  }
}

function applyConnection(state: ConnectionState): void {
  const wasConnected = connectionStore.get().state === "connected";
  connectionStore.set(state);
  if (state.state === "connected" && !wasConnected) {
    // Live output streamed while disconnected is not replayed.
    liveStore.set((live) => Object.fromEntries(Object.entries(live).map(([id, l]) => [id, { ...l, gap: true }])));
    void loadHistory();
  }
}

let started = false;

export async function startCoreSync(): Promise<void> {
  if (started) return;
  started = true;
  if (!runtimeAvailable()) {
    connectionStore.set({
      state: "unavailable",
      error: "Not running inside the Switchyard desktop app. Start it with `npm run dev`.",
    });
    return;
  }
  await onCoreEvent((event) => {
    eventFeedStore.set((feed) => ({ ...feed, events: mergeEvents(feed.events, [event], EVENT_BUFFER) }));
    for (const listener of eventListeners) listener(event);
  });
  await onLive((output) => liveStore.set((state) => applyLive(state, output)));
  await onConnection(applyConnection);
  // The bridge may have connected before the listener was registered.
  applyConnection(await getConnection());
}
