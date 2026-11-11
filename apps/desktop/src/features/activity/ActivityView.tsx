import { useMemo, useState } from "react";

import { EmptyState, ErrorNotice, PageHeader, Panel } from "../../components/ui";
import { useNow } from "../../lib/useNow";
import { useEventFeed } from "../../stores/core";
import { EventList } from "./EventList";

export function ActivityView() {
  const feed = useEventFeed();
  const now = useNow(5_000);
  const [filter, setFilter] = useState("");
  const types = useMemo(() => [...new Set(feed.events.map((e) => e.event_type))].sort(), [feed.events]);
  const shown = filter ? feed.events.filter((e) => e.event_type === filter) : feed.events;

  return (
    <div className="page">
      <PageHeader
        title="Activity"
        subtitle="Live view of Switchyard Core's durable event journal."
        actions={
          <select className="input select" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter by event type">
            <option value="">All event types</option>
            {types.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
        }
      />
      {feed.error ? <ErrorNotice title="Cannot load event history" message={feed.error} /> : null}
      <Panel title={`Events (${shown.length})`}>
        {shown.length === 0 ? (
          <EmptyState icon="activity" title={feed.loaded ? "No events yet" : "Waiting for Core"}>
            Agent activity will appear here once agent adapters are available. Today the journal records Core lifecycle
            and project registration events.
          </EmptyState>
        ) : (
          <EventList events={shown} now={now} />
        )}
      </Panel>
    </div>
  );
}
