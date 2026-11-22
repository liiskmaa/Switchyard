import { useEffect, useRef, useState } from "react";

import { startCore } from "../lib/core";
import { formatTimestamp } from "../lib/format";
import { useConnection } from "../stores/core";
import { CoreControls, useCoreWarnings } from "./CoreControls";
import { Dot, type Tone } from "./ui";

export function connectionSummary(state: ReturnType<typeof useConnection>): { tone: Tone; label: string; detail: string } {
  switch (state.state) {
    case "connected":
      return { tone: "ok", label: "Core connected", detail: `v${state.core.version} · pid ${state.core.pid}` };
    case "connecting":
      return { tone: "warn", label: "Connecting to Core", detail: state.attempt > 1 ? `attempt ${state.attempt}` : "" };
    case "disconnected":
      if (state.incompatible) return { tone: "error", label: "Incompatible Core", detail: "click to restart it" };
      return {
        tone: "error",
        label: state.launched_core ? "Starting Core" : "Core unavailable",
        detail: `retrying in ${Math.ceil(state.retry_in_ms / 100) / 10}s`,
      };
    case "stopped":
      return { tone: "warn", label: "Core stopped", detail: "start it under Settings" };
    case "unavailable":
      return { tone: "error", label: "No desktop runtime", detail: "" };
  }
}

export function StatusIndicator() {
  const state = useConnection();
  const { tone, label, detail } = connectionSummary(state);
  const { env, warnings } = useCoreWarnings();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent | KeyboardEvent) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !ref.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open]);
  const title = state.state === "disconnected" || state.state === "unavailable" ? state.error : "Switchyard Core: start, restart or stop";
  return (
    <div className="status-wrap" ref={ref}>
      {open ? (
        <div className="status-pop" role="dialog" aria-label="Switchyard Core">
          <div className="status-pop-head">
            <strong>Switchyard Core</strong>
            <span className="muted small">{label}</span>
          </div>
          <dl className="status-versions small">
            <dt>Core</dt>
            <dd className="mono">{state.state === "connected" ? `v${state.core.version} · protocol ${state.protocol_version}` : "—"}</dd>
            <dt>App</dt>
            <dd className="mono">{env ? `v${env.desktop_version}` : "—"}</dd>
            <dt>Started</dt>
            <dd>{state.state === "connected" ? formatTimestamp(state.core.started_at_ms) : "—"}</dd>
          </dl>
          {state.state === "disconnected" && !state.incompatible ? <p className="muted small">{state.error}</p> : null}
          <CoreControls compact onDone={() => setOpen(false)} />
        </div>
      ) : null}
      <button
        type="button"
        className="status-indicator"
        title={title}
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={() => setOpen((o) => !o)}
      >
        <Dot tone={warnings.length > 0 && tone === "ok" ? "warn" : tone} />
        <div>
          <div className="status-label" role="status" aria-live="polite">
            {label}
            {warnings.length > 0 ? <span className="status-warn"> · needs attention</span> : null}
          </div>
          {detail ? <div className="status-detail">{detail}</div> : null}
        </div>
      </button>
      {/* Always visible: Start when Core is down, Restart and Stop when it runs (confirmed in the panel). */}
      <div className="status-actions">
        {state.state === "connected" || (state.state === "disconnected" && state.incompatible) ? (
          <>
            <button type="button" className={`status-action${warnings.length > 0 ? " attention" : ""}`} onClick={() => setOpen(true)}>
              Restart
            </button>
            <button type="button" className="status-action" onClick={() => setOpen(true)}>
              Stop
            </button>
          </>
        ) : state.state === "stopped" || state.state === "disconnected" ? (
          <button type="button" className="status-action attention" onClick={() => void startCore()}>
            Start
          </button>
        ) : null}
      </div>
    </div>
  );
}
