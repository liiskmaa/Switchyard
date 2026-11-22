import { useCallback, useEffect, useRef, useState } from "react";

import { toCoreError, type CoreRequestError } from "../lib/core";
import type { EventEnvelope } from "../types/protocol";
import { connectionKey, useConnection, useCoreEvents } from "./core";

export interface QueryState<T> {
  data: T | null;
  error: CoreRequestError | null;
  loading: boolean;
  refresh: () => void;
}

interface Options {
  /** Re-run the query when a live event matches. */
  refreshOn?: (event: EventEnvelope) => boolean;
  /** Poll interval while connected. */
  intervalMs?: number;
  enabled?: boolean;
}

/** Matching events within this window cause one refresh, not one each. */
const EVENT_DEBOUNCE_MS = 250;
/** Retry delay when Core says the connection has too many requests in flight. */
const OVERLOADED_RETRY_MS = 600;

/**
 * Runs `fetcher` whenever Core (re)connects, when `key` changes, on matching
 * events and optionally on an interval. Results from superseded runs are
 * discarded.
 *
 * Core limits the requests in flight per connection, and a busy agent emits
 * many events: so each query runs at most one fetch at a time (a refresh
 * asked for meanwhile runs once afterwards), bursts of events collapse into
 * one refresh, and an "overloaded" answer is retried quietly instead of
 * replacing the data with an error.
 */
export function useCoreQuery<T>(key: string, fetcher: () => Promise<T>, options: Options = {}): QueryState<T> {
  const { refreshOn, intervalMs, enabled = true } = options;
  const connection = useConnection();
  const connected = connectionKey(connection);
  const [state, setState] = useState<{ data: T | null; error: CoreRequestError | null; loading: boolean }>({
    data: null,
    error: null,
    loading: false,
  });
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;
  const generation = useRef(0);
  const inFlight = useRef(false);
  const again = useRef(false);
  const timer = useRef<number | null>(null);

  const run = useCallback(() => {
    if (!connected || !enabled) return;
    if (inFlight.current) {
      again.current = true;
      return;
    }
    inFlight.current = true;
    const current = ++generation.current;
    setState((s) => ({ ...s, loading: true }));
    const settle = () => {
      inFlight.current = false;
      if (again.current) {
        again.current = false;
        run();
      }
    };
    fetcherRef.current().then(
      (data) => {
        if (current === generation.current) setState({ data, error: null, loading: false });
        settle();
      },
      (error: unknown) => {
        const e = toCoreError(error);
        if (e.code === "overloaded") {
          // Not a real failure: try again shortly and keep what is shown.
          again.current = false;
          inFlight.current = false;
          window.setTimeout(run, OVERLOADED_RETRY_MS + Math.random() * 400);
          return;
        }
        if (current === generation.current) setState((s) => ({ ...s, error: e, loading: false }));
        settle();
      },
    );
  }, [connected, enabled]);

  const later = useCallback(() => {
    if (timer.current !== null) return;
    timer.current = window.setTimeout(() => {
      timer.current = null;
      run();
    }, EVENT_DEBOUNCE_MS);
  }, [run]);
  useEffect(
    () => () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    },
    [],
  );

  useEffect(() => {
    setState({ data: null, error: null, loading: false });
    // A new key must not wait for (or be dropped by) the previous key's fetch.
    inFlight.current = false;
    again.current = false;
    run();
  }, [run, key]);

  useEffect(() => {
    if (!intervalMs || !connected) return;
    const timer = window.setInterval(run, intervalMs);
    return () => window.clearInterval(timer);
  }, [run, intervalMs, connected]);

  const onEvent = useCallback(
    (event: EventEnvelope) => {
      if (refreshOn?.(event)) later();
    },
    [refreshOn, later],
  );
  useCoreEvents(onEvent);

  return { ...state, refresh: run };
}
