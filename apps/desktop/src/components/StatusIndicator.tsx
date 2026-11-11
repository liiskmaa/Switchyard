import { useConnection } from "../stores/core";
import { Dot, type Tone } from "./ui";

export function connectionSummary(state: ReturnType<typeof useConnection>): { tone: Tone; label: string; detail: string } {
  switch (state.state) {
    case "connected":
      return { tone: "ok", label: "Core connected", detail: `v${state.core.version} · pid ${state.core.pid}` };
    case "connecting":
      return { tone: "warn", label: "Connecting to Core", detail: state.attempt > 1 ? `attempt ${state.attempt}` : "" };
    case "disconnected":
      return {
        tone: "error",
        label: state.launched_core ? "Starting Core" : "Core unavailable",
        detail: `retrying in ${Math.ceil(state.retry_in_ms / 100) / 10}s`,
      };
    case "unavailable":
      return { tone: "error", label: "No desktop runtime", detail: "" };
  }
}

export function StatusIndicator() {
  const state = useConnection();
  const { tone, label, detail } = connectionSummary(state);
  const title = state.state === "disconnected" || state.state === "unavailable" ? state.error : undefined;
  return (
    <div className="status-indicator" title={title} role="status" aria-live="polite">
      <Dot tone={tone} />
      <div>
        <div className="status-label">{label}</div>
        {detail ? <div className="status-detail">{detail}</div> : null}
      </div>
    </div>
  );
}
