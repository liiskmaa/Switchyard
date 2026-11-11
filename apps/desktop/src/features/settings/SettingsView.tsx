import { type ReactNode, useEffect, useState } from "react";

import { Badge, Button, ErrorNotice, KeyValue, PageHeader, Panel } from "../../components/ui";
import { getEnvironment, reconnectNow, runtimeAvailable, toCoreError } from "../../lib/core";
import { formatTimestamp } from "../../lib/format";
import { useConnection } from "../../stores/core";
import type { CoreEnvironment } from "../../types/protocol";
import { connectionSummary } from "../../components/StatusIndicator";

export function SettingsView() {
  const connection = useConnection();
  const [env, setEnv] = useState<CoreEnvironment | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!runtimeAvailable()) return;
    getEnvironment().then(setEnv, (e: unknown) => setError(toCoreError(e).message));
  }, []);

  const summary = connectionSummary(connection);
  const path = (p: string | null | undefined) => (p ? <code className="path">{p}</code> : <span className="muted">not found</span>);

  return (
    <div className="page">
      <PageHeader title="Settings" subtitle="Local runtime configuration. Values are read from the running system." />
      {error ? <ErrorNotice title="Cannot read environment" message={error} /> : null}
      <Panel
        title="Switchyard Core"
        actions={
          <Button variant="ghost" icon="refresh" onClick={() => void reconnectNow()} disabled={!runtimeAvailable()}>
            Reconnect
          </Button>
        }
      >
        <KeyValue
          rows={[
            ["Connection", <Badge tone={summary.tone}>{summary.label}</Badge>],
            ...(connection.state === "connected"
              ? ([
                  ["Core version", connection.core.version],
                  ["Protocol", `v${connection.protocol_version}`],
                  ["Process ID", String(connection.core.pid)],
                  ["Started", formatTimestamp(connection.core.started_at_ms)],
                  ["Instance", <code>{connection.core.instance_id}</code>],
                ] as [string, ReactNode][])
              : ([["Last error", connection.state === "connecting" ? "—" : connection.error]] as [string, ReactNode][])),
          ]}
        />
      </Panel>
      <Panel title="Paths">
        <KeyValue
          rows={[
            ["Data directory", path(env?.data_dir)],
            ["Socket", path(env?.socket_path)],
            ["Database", path(env?.database_path)],
            ["Core executable", path(env?.core_binary)],
            ["Core log", path(env?.core_log)],
            ["Auto-start Core", env ? (env.auto_launch ? "Enabled" : "Disabled (SWITCHYARD_AUTO_LAUNCH_CORE=0)") : "—"],
            ["Desktop version", env?.desktop_version ?? "—"],
          ]}
        />
        <p className="muted small">
          Override with <code>SWITCHYARD_DATA_DIR</code>, <code>SWITCHYARD_SOCKET</code> and <code>SWITCHYARD_CORE_BIN</code>{" "}
          before launching the app.
        </p>
      </Panel>
    </div>
  );
}
