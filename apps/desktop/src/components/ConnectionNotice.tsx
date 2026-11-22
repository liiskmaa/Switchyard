import { reconnectNow, startCore } from "../lib/core";
import { useConnection } from "../stores/core";
import { Icon } from "./Icon";
import { Button } from "./ui";

/** Banner shown above every view while Core is not connected. */
export function ConnectionNotice() {
  const state = useConnection();
  if (state.state === "connected") return null;
  if (state.state === "connecting") {
    return (
      <div className="banner banner-warn" role="status">
        <Icon name="refresh" /> Connecting to Switchyard Core…
      </div>
    );
  }
  if (state.state === "stopped") {
    return (
      <div className="banner banner-warn" role="status">
        <Icon name="alert" />
        <span className="banner-text">
          Switchyard Core is stopped. Agents are not running; nothing changes until you start it.
        </span>
        <Button variant="ghost" icon="refresh" onClick={() => void startCore()}>
          Start Core
        </Button>
      </div>
    );
  }
  return (
    <div className="banner banner-error" role="alert">
      <Icon name="alert" />
      <span className="banner-text">
        {state.state === "disconnected" && state.launched_core ? "Starting Switchyard Core. " : "Switchyard Core is not reachable. "}
        <span className="muted">{state.error}</span>
      </span>
      {state.state === "disconnected" ? (
        <Button variant="ghost" icon="refresh" onClick={() => void reconnectNow()}>
          Retry now
        </Button>
      ) : null}
    </div>
  );
}
