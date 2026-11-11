import { reconnectNow } from "../lib/core";
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
