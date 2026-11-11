import { Badge, type Tone } from "../../components/ui";
import { describeEvent, formatRelative, formatTimestamp } from "../../lib/format";
import type { EventEnvelope } from "../../types/protocol";

function tone(type: string): Tone {
  if (type.startsWith("core.")) return "neutral";
  if (type.endsWith(".removed")) return "warn";
  return "accent";
}

export function EventList({ events, now, compact = false }: { events: EventEnvelope[]; now: number; compact?: boolean }) {
  return (
    <ol className={`events${compact ? " compact" : ""}`}>
      {[...events].reverse().map((e) => (
        <li key={e.seq} className="event">
          <span className="event-seq mono">#{e.seq}</span>
          <Badge tone={tone(e.event_type)}>{e.event_type}</Badge>
          <span className="event-text">{describeEvent(e)}</span>
          <time className="event-time" dateTime={new Date(e.occurred_at_ms).toISOString()} title={formatTimestamp(e.occurred_at_ms)}>
            {formatRelative(e.occurred_at_ms, now)}
          </time>
        </li>
      ))}
    </ol>
  );
}
