import { useEffect, useState } from "react";

import { getEnvironment, restartCore, runtimeAvailable, startCore, stopCore, toCoreError } from "../lib/core";
import { formatTimestamp } from "../lib/format";
import { useConnection } from "../stores/core";
import type { CoreEnvironment } from "../types/protocol";
import { Button, ErrorNotice } from "./ui";

type Pending = "stop" | "restart" | null;

/** What is wrong with the Core the app talks to, if anything, in one sentence each. */
export function useCoreWarnings(): { env: CoreEnvironment | null; warnings: string[]; refresh: () => void } {
  const connection = useConnection();
  const [env, setEnv] = useState<CoreEnvironment | null>(null);
  const refresh = () => {
    if (!runtimeAvailable()) return;
    getEnvironment().then(setEnv, () => setEnv(null));
  };
  // The build on disk changes while the app runs; read it again on every (re)connect.
  const instance = connection.state === "connected" ? connection.core.instance_id : connection.state;
  useEffect(refresh, [instance]);
  const warnings: string[] = [];
  if (connection.state === "disconnected" && connection.incompatible) {
    warnings.push("The running Core is a different version that this app cannot talk to. Restart it to start the matching build.");
  }
  if (
    connection.state === "connected" &&
    env?.core_binary_modified_ms != null &&
    env.core_binary_modified_ms > connection.core.started_at_ms
  ) {
    warnings.push(
      `A newer Core build is on disk (built ${formatTimestamp(env.core_binary_modified_ms)}; the running Core started ${formatTimestamp(connection.core.started_at_ms)}). Restart Core to use it.`,
    );
  }
  if (connection.state === "connected" && env && connection.core.version !== env.desktop_version) {
    warnings.push(`Core ${connection.core.version} and the desktop app ${env.desktop_version} are different versions.`);
  }
  return { env, warnings, refresh };
}

/**
 * Start, restart and stop Switchyard Core. Stopping and restarting are
 * confirmed in place, because they stop running agents.
 */
export function CoreControls({ compact = false, onDone }: { compact?: boolean; onDone?: () => void }) {
  const connection = useConnection();
  const { warnings, refresh } = useCoreWarnings();
  const [confirming, setConfirming] = useState<Pending>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const available = runtimeAvailable();
  const connected = connection.state === "connected";
  const incompatible = connection.state === "disconnected" && connection.incompatible === true;
  const running = connected || incompatible;

  const run = (label: string, f: () => Promise<void>) =>
    void (async () => {
      setBusy(label);
      setError(null);
      setConfirming(null);
      try {
        await f();
        onDone?.();
      } catch (e) {
        setError(toCoreError(e).message);
      } finally {
        setBusy(null);
        refresh();
      }
    })();

  return (
    <div className={`core-controls${compact ? " compact" : ""}`}>
      {warnings.map((w) => (
        <div key={w} className="notice notice-warn" role="status">
          <div>
            <p>{w}</p>
          </div>
        </div>
      ))}
      {error ? <ErrorNotice title="Core action failed" message={error} /> : null}
      {confirming ? (
        <div className="notice notice-warn" role="status">
          <div>
            <strong>{confirming === "stop" ? "Stop Switchyard Core?" : "Restart Switchyard Core?"}</strong>
            <p>
              Running agents stop and pending approvals are denied; sessions can be resumed afterwards, and workflows wait for
              you to resume them.{confirming === "stop" ? " Nothing starts Core again until you press Start." : ""}
            </p>
          </div>
        </div>
      ) : null}
      <div className="form-actions start">
        {confirming ? (
          <>
            <Button
              variant="danger"
              disabled={busy !== null}
              onClick={() => run(confirming, confirming === "stop" ? stopCore : restartCore)}
            >
              {confirming === "stop" ? "Stop Core" : "Restart Core"}
            </Button>
            <Button variant="ghost" onClick={() => setConfirming(null)}>
              Cancel
            </Button>
          </>
        ) : (
          <>
            {!running ? (
              <Button variant="primary" disabled={!available || busy !== null} onClick={() => run("start", startCore)}>
                {busy === "start" ? "Starting…" : "Start"}
              </Button>
            ) : null}
            <Button
              variant={warnings.length > 0 ? "primary" : "default"}
              disabled={!available || busy !== null || !running}
              onClick={() => setConfirming("restart")}
            >
              {busy === "restart" ? "Restarting…" : "Restart…"}
            </Button>
            <Button variant="ghost" disabled={!available || busy !== null || !running} onClick={() => setConfirming("stop")}>
              {busy === "stop" ? "Stopping…" : "Stop…"}
            </Button>
          </>
        )}
      </div>
    </div>
  );
}
