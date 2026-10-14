/**
 * The D16 deny-list: relay admin endpoints and Daytona-API-key patterns, and
 * nothing else. Checked before every tool call (D64). Also used, via the
 * arbos-shell wrapper, for Prime's bash(): keep the two lists identical.
 */

export interface DenyRule {
  name: string;
  why: string;
  pattern: RegExp;
}

export const DENY_RULES: DenyRule[] = [
  {
    name: "daytona-api-key",
    why: "Daytona API keys never exist in a box; anything that looks like one is refused",
    pattern: /(dtn_[A-Za-z0-9]{20,}|DAYTONA_API_KEY|api\.daytona\.io\/api\/(api-keys|organizations))/,
  },
  {
    name: "relay-admin",
    why: "relay admin endpoints are not reachable with a box token",
    pattern: /\/(admin|v1\/admin|pair\/new|devices)(\/|\?|\s|$)/,
  },
  {
    name: "arbos-state-tamper",
    why: "agentd's own state and audit log are not writable by the model",
    pattern: /(^|\s|['"])\/work\/\.arbos\/(agentd|audit\.jsonl)/,
  },
];

export interface DenyHit {
  rule: string;
  why: string;
}

/** Flatten the tool input to one string and test the rules against it. */
export function matchDeny(toolName: string, input: Record<string, unknown>): DenyHit | null {
  const hay = `${toolName} ${JSON.stringify(input ?? {})}`;
  // The shared audit path is written by this extension, not by the model's writes/edits.
  for (const rule of DENY_RULES) {
    if (rule.name === "arbos-state-tamper" && !["bash", "write", "edit"].includes(toolName)) continue;
    if (rule.pattern.test(hay)) return { rule: rule.name, why: rule.why };
  }
  return null;
}
