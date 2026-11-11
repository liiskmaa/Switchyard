// Process-wide Core state: connection status and the live event feed.
// Started once from main.tsx; components read it through hooks.

import { useEffect } from "react";

import { coreRequest, getConnection, onConnection, onCoreEvent, runtimeAvailable, toCoreError } from "../lib/core";
import { mergeEvents } from "../lib/format";
import type { ConnectionState, EventEnvelope } from "../types/protocol";
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
  if (state.state === "connected" && !wasConnected) void loadHistory();
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
  await onConnection(applyConnection);
  // The bridge may have connected before the listener was registered.
  applyConnection(await getConnection());
}
