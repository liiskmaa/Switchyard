import type { AgentProvider, Capabilities, Support } from "../types/protocol";

const NAMES: Record<string, { full: string; short: string; nativeId: string; login: string }> = {
  claude_code: { full: "Claude Code", short: "Claude", nativeId: "Claude session", login: "claude auth login" },
  codex: { full: "Codex", short: "Codex", nativeId: "Codex thread", login: "codex login" },
  gemini: {
    full: "Gemini CLI",
    short: "Gemini",
    nativeId: "Gemini session",
    login: "put GEMINI_API_KEY=… in ~/.gemini/.env",
  },
};

/** "Claude Code" / "Codex"; unknown providers are shown as reported. */
export const providerName = (provider: string): string => NAMES[provider]?.full ?? provider;
/** "Claude" / "Codex", for sentences ("Claude will start it…"). */
export const providerShort = (provider: string): string => NAMES[provider]?.short ?? provider;
/** Label for the agent's own conversation id. */
export const nativeIdLabel = (provider: string): string => NAMES[provider]?.nativeId ?? "Native session";
/** The command that signs the CLI in. */
export const loginCommand = (provider: string): string | null => NAMES[provider]?.login ?? null;

export const PROVIDERS: AgentProvider[] = ["claude_code", "codex", "gemini"];

/** Only `supported` counts; `unknown` is treated as not available. */
export const supported = (s: Support | undefined): boolean => s === "supported";

export const SUPPORT_LABEL: Record<Support, string> = {
  supported: "Supported",
  unsupported: "Not supported",
  unknown: "Not verified",
};

export const SUPPORT_MARK: Record<Support, string> = { supported: "✓", unsupported: "✕", unknown: "?" };

export const CAPABILITY_LABELS: [keyof Capabilities, string][] = [
  ["multi_turn", "Multiple turns per session"],
  ["streaming_text", "Streaming output"],
  ["tool_activity", "Tool activity"],
  ["permission_requests", "Permission requests routed to Switchyard"],
  ["denial_reason_to_agent", "Agent is told why a request was denied"],
  ["file_read_tool", "File reads as a separate tool"],
  ["interrupt_turn", "Interrupt a running turn"],
  ["queue_message_during_turn", "Queue a message for the next turn"],
  ["steer_running_turn", "Change a running turn"],
  ["resume_session", "Resume a finished session"],
  ["usage_tokens", "Token usage"],
  ["cost", "Cost"],
];

/**
 * Whether a denial note reaches the agent. Mirrors the adapters'
 * `denial_reason_to_agent` capability (Claude: yes; Codex: only "rejected").
 */
export const denialReasonReachesAgent = (provider: string): boolean => provider === "claude_code";
