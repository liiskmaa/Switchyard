// The only module that talks to the desktop runtime. Everything goes through
// the Tauri bridge to Switchyard Core; the UI never touches Core's socket,
// SQLite or agent processes.

import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  ConnectionState,
  CoreEnvironment,
  CoreMethod,
  CoreMethods,
  EventEnvelope,
  LiveOutput,
} from "../types/protocol";

export class CoreRequestError extends Error {
  readonly code: string;
  readonly data: unknown;

  constructor(code: string, message: string, data?: unknown) {
    super(message);
    this.name = "CoreRequestError";
    this.code = code;
    this.data = data;
  }
}

export function toCoreError(error: unknown): CoreRequestError {
  if (error instanceof CoreRequestError) return error;
  if (typeof error === "object" && error !== null && "code" in error && "message" in error) {
    const { code, message, data } = error as { code: unknown; message: unknown; data?: unknown };
    return new CoreRequestError(String(code), String(message), data);
  }
  if (error instanceof Error) return new CoreRequestError("client_error", error.message);
  return new CoreRequestError("client_error", String(error));
}

export const runtimeAvailable = (): boolean => isTauri();

export async function coreRequest<M extends CoreMethod>(
  method: M,
  params: CoreMethods[M]["params"],
): Promise<CoreMethods[M]["result"]> {
  try {
    return await invoke<CoreMethods[M]["result"]>("core_request", { method, params });
  } catch (error) {
    throw toCoreError(error);
  }
}

export const getConnection = (): Promise<ConnectionState> => invoke<ConnectionState>("core_connection");
export const getEnvironment = (): Promise<CoreEnvironment> => invoke<CoreEnvironment>("core_environment");
export const reconnectNow = (): Promise<void> => invoke<void>("core_reconnect");
/** Stops Core gracefully; nothing restarts it until {@link startCore}. */
export const stopCore = (): Promise<void> => invoke<void>("core_stop");
export const startCore = (): Promise<void> => invoke<void>("core_start");
/** Stops Core and starts the executable on disk again (picks up a new build). */
export const restartCore = (): Promise<void> => invoke<void>("core_restart");

export const onConnection = (handler: (state: ConnectionState) => void): Promise<UnlistenFn> =>
  listen<ConnectionState>("core://connection", (e) => handler(e.payload));

export const onCoreEvent = (handler: (event: EventEnvelope) => void): Promise<UnlistenFn> =>
  listen<EventEnvelope>("core://event", (e) => handler(e.payload));

export const onLive = (handler: (output: LiveOutput) => void): Promise<UnlistenFn> =>
  listen<LiveOutput>("core://live", (e) => handler(e.payload));
