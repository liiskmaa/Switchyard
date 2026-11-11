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

/**
 * Runs `fetcher` whenever Core (re)connects, when `key` changes, on matching
 * events and optionally on an interval. Results from superseded runs are
 * discarded.
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

  const run = useCallback(() => {
    if (!connected || !enabled) return;
    const current = ++generation.current;
    setState((s) => ({ ...s, loading: true }));
    fetcherRef.current().then(
      (data) => {
        if (current === generation.current) setState({ data, error: null, loading: false });
      },
      (error: unknown) => {
        if (current === generation.current) setState((s) => ({ ...s, error: toCoreError(error), loading: false }));
      },
    );
  }, [connected, enabled]);

  useEffect(() => {
    setState({ data: null, error: null, loading: false });
    run();
  }, [run, key]);

  useEffect(() => {
    if (!intervalMs || !connected) return;
    const timer = window.setInterval(run, intervalMs);
    return () => window.clearInterval(timer);
  }, [run, intervalMs, connected]);

  const onEvent = useCallback(
    (event: EventEnvelope) => {
      if (refreshOn?.(event)) run();
    },
    [refreshOn, run],
  );
  useCoreEvents(onEvent);

  return { ...state, refresh: run };
}
