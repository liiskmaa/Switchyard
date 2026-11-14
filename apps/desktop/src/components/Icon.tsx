// Inline stroke icons (no icon dependency). 16px grid, currentColor.

const paths = {
  dashboard: "M2.5 2.5h4.5v5h-4.5zM9 2.5h4.5v3H9zM9 7.5h4.5v6H9zM2.5 9.5h4.5v4h-4.5z",
  projects: "M2 4.5c0-.8.6-1.5 1.5-1.5h3l1.5 1.5h4.5c.8 0 1.5.7 1.5 1.5v6c0 .8-.7 1.5-1.5 1.5h-9C2.6 13.5 2 12.8 2 12z",
  agents: "M5 6.5a3 3 0 1 0 6 0a3 3 0 1 0-6 0M2.5 14c.8-2.3 3-3.5 5.5-3.5s4.7 1.2 5.5 3.5",
  activity: "M1.5 8.5h3l2-5 3 9 2-4h3",
  approvals: "M3 8.5l3 3 7-7",
  tasks: "M2.5 4l1.5 1.5L6.5 3M2.5 9l1.5 1.5L6.5 8M8.5 4.5h5M8.5 9.5h5M8.5 13h5",
  policy: "M8 1.8l5 2v4c0 3.1-2.1 5.4-5 6.4c-2.9-1-5-3.3-5-6.4v-4zM5.8 8l1.6 1.6 3-3.2",
  settings:
    "M8 5.5a2.5 2.5 0 1 0 0 5a2.5 2.5 0 1 0 0-5M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4",
  plus: "M8 3v10M3 8h10",
  refresh: "M13 3.5v3h-3M3 12.5v-3h3M12.5 6.5A5 5 0 0 0 3.6 5M3.5 9.5a5 5 0 0 0 8.9 1.5",
  branch: "M5 2.5v11M5 10.5c0-3 6-2 6-5.5M11 2.5a1.5 1.5 0 1 0 0 3a1.5 1.5 0 1 0 0-3",
  folder: "M2 4.5c0-.8.6-1.5 1.5-1.5h3l1.5 1.5h4.5c.8 0 1.5.7 1.5 1.5v6c0 .8-.7 1.5-1.5 1.5h-9C2.6 13.5 2 12.8 2 12z",
  alert: "M8 2l6.5 11.5h-13zM8 6.5v3M8 11.5v.01",
  close: "M4 4l8 8M12 4l-8 8",
} as const;

export type IconName = keyof typeof paths;

export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={paths[name]} />
    </svg>
  );
}

export function BrandMark({ size = 20 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 20 20" aria-hidden="true">
      <path d="M7 17V3" stroke="var(--text)" strokeWidth={2} strokeLinecap="round" />
      <path d="M7 14v-2l6-6V3" stroke="var(--accent)" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" fill="none" />
    </svg>
  );
}
