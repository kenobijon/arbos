/**
 * arbos-pi-ext: the one pi extension Arbos loads into the main agent (and the
 * pi fallback subagent). Design D64/D65/D66, arbos-design.md §4.4.1.
 *
 * 1. `tool_call` hook: deny-list veto + audit append BEFORE execution, act-turn call counting.
 * 2. `ask_user` tool → agentd raises InputRequested (S8).
 * 3. `route` custom tool, validated by agentd (one retry, then `reply`).
 * 4. `tool_result` offload (> 4,000 chars → blob + handle), `peek`, working-set section.
 *
 * Everything that needs agentd goes over the loopback hook API (ARBOS_HOOK_URL).
 * The extension keeps working with no agentd (local pi runs): hooks degrade to no-ops.
 */

import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

import { DENY_RULES, matchDeny } from "./deny.ts";
import { ROUTE_PARAMS, type RouteArgs } from "./route-schema.ts";

const STATE_DIR = process.env.ARBOS_STATE_DIR ?? join(process.env.HOME ?? "/tmp", ".arbos");
const AUDIT_PATH = process.env.ARBOS_AUDIT_PATH ?? join(STATE_DIR, "audit.jsonl");
const BLOB_DIR = process.env.ARBOS_BLOB_DIR ?? join(STATE_DIR, "blobs");
const HOOK_URL = process.env.ARBOS_HOOK_URL ?? "";
const HOOK_TOKEN = process.env.ARBOS_MCP_TOKEN ?? "";
const SESSION_ROLE = process.env.ARBOS_SESSION_ROLE ?? "main"; // main | subagent
const OFFLOAD_CHARS = Number(process.env.ARBOS_OFFLOAD_CHARS ?? 4000);
const ACT_MAX_CALLS = Number(process.env.ARBOS_ACT_MAX_CALLS ?? 8);
const HEAD_LINES = 20;
const TAIL_LINES = 10;
const ROUTE_PREFIX = "(Arbos: call `route` first.) ";

type HookResponse = { ok: boolean; error?: string; terminate?: boolean; text?: string; [k: string]: unknown };

async function hook(path: string, body: unknown): Promise<HookResponse | null> {
  if (!HOOK_URL) return null;
  try {
    const res = await fetch(`${HOOK_URL}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${HOOK_TOKEN}` },
      body: JSON.stringify(body),
    });
    return (await res.json()) as HookResponse;
  } catch (err) {
    return { ok: false, error: `agentd unreachable: ${String(err)}` };
  }
}

function ensureDir(dir: string): void {
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
}

function audit(entry: Record<string, unknown>): void {
  try {
    ensureDir(STATE_DIR);
    appendFileSync(AUDIT_PATH, `${JSON.stringify({ ts: Date.now(), ...entry })}\n`);
  } catch {
    // The audit file is best-effort on a read-only test host; the hook API mirrors it.
  }
}

function inputHash(input: unknown): string {
  return createHash("sha256").update(JSON.stringify(input ?? null)).digest("hex").slice(0, 16);
}

interface RouteDetails {
  action?: string;
  fallback?: string;
  error?: string;
}

interface Handle {
  id: string;
  kind: string;
  chars: number;
  lines: number;
  path: string;
  note: string;
  createdAt: number;
}

export default function arbosPiExt(pi: ExtensionAPI) {
  // Per-turn state. Reset at every run start.
  let actCalls = 0;
  let routeAttempts = 0;
  let routed = false;
  let routeCorrections = 0;
  let correcting = false;
  let sessionId = "";
  const handles = new Map<string, Handle>();
  const scratch = new Map<string, string>();

  pi.on("session_start", async (_event, ctx) => {
    sessionId = ctx.sessionManager.getSessionId?.() ?? "";
    for (const entry of ctx.sessionManager.getBranch()) {
      if (entry.type === "custom" && entry.customType === "arbos-handle") {
        const h = entry.data as Handle;
        handles.set(h.id, h);
      }
      if (entry.type === "custom" && entry.customType === "arbos-remember") {
        const d = entry.data as { name: string; value: string | null };
        if (d.value === null) scratch.delete(d.name);
        else scratch.set(d.name, d.value);
      }
    }
  });

  // ---- 4b. working-set section, refreshed every run and after compaction -----------------
  pi.on("before_agent_start", async (event) => {
    actCalls = 0;
    routeAttempts = 0;
    routed = false;
    routeCorrections = 0;
    correcting = false;
    const lines: string[] = [];
    for (const h of handles.values()) {
      lines.push(`- ${h.id} (${h.kind}, ${h.chars.toLocaleString()} chars, ${h.lines} lines): ${h.note}`);
    }
    for (const [k, v] of scratch) {
      lines.push(`- remember:${k} = ${v.length > 120 ? `${v.slice(0, 117)}…` : v}`);
    }
    const remote = await hook("/hook/working-set", { session_id: sessionId, role: SESSION_ROLE });
    if (remote?.ok && typeof remote.text === "string" && remote.text.trim()) lines.push(remote.text.trim());
    const capped = lines.slice(0, 40); // ≈ 600 tokens (design §6.6 item 2b)
    if (capped.length > 0) {
      event.systemPromptOptions.sections["arbos-working-set"] =
        `Named handles and notes of this chat. Use peek(handle) for slices; never ask to re-run them.\n${capped.join("\n")}`;
    }
  });

  // ---- 1. deny-list veto + audit before execution + act counting --------------------------
  pi.on("tool_call", async (event) => {
    const denied = matchDeny(event.toolName, event.input as Record<string, unknown>);
    audit({
      kind: "tool_call",
      session: sessionId,
      role: SESSION_ROLE,
      tool: event.toolName,
      call_id: event.toolCallId,
      input_hash: inputHash(event.input),
      input_preview: JSON.stringify(event.input).slice(0, 200),
      denied: denied?.rule ?? null,
    });
    if (denied) {
      void hook("/hook/deny", { session_id: sessionId, tool: event.toolName, rule: denied.rule, call_id: event.toolCallId });
      return { block: true, reason: `Blocked by the Arbos deny-list (${denied.rule}): ${denied.why}` };
    }
    if (SESSION_ROLE === "main" && event.toolName !== "route" && event.toolName !== "ask_user" && !event.parentToolCallId) {
      actCalls += 1;
      if (actCalls > ACT_MAX_CALLS) {
        void hook("/hook/act-limit", { session_id: sessionId, calls: actCalls, call_id: event.toolCallId });
        return {
          block: true,
          terminate: true,
          reason: `Act limit reached (${ACT_MAX_CALLS} tool calls). Stop now and summarize what you did and what is left; Arbos hands the rest to a subagent.`,
        };
      }
    }
    return undefined;
  });

  // ---- 4a. offload hook: big tool results become handles -----------------------------------
  pi.on("tool_result", async (event) => {
    const textBlocks = event.content.filter((c) => c.type === "text") as { type: "text"; text: string }[];
    const full = textBlocks.map((b) => b.text).join("\n");
    if (full.length <= OFFLOAD_CHARS) return undefined;
    ensureDir(BLOB_DIR);
    const sha = createHash("sha256").update(full).digest("hex");
    const id = `h_${sha.slice(0, 8)}`;
    const path = join(BLOB_DIR, sha);
    if (!existsSync(path)) writeFileSync(path, full);
    const allLines = full.split("\n");
    const handle: Handle = {
      id,
      kind: `${event.toolName} output`,
      chars: full.length,
      lines: allLines.length,
      path,
      note: `${event.toolName} ${JSON.stringify(event.input).slice(0, 80)}`,
      createdAt: Date.now(),
    };
    handles.set(id, handle);
    pi.appendEntry("arbos-handle", handle);
    void hook("/hook/handle", { session_id: sessionId, handle });
    const head = allLines.slice(0, HEAD_LINES).join("\n");
    const tail = allLines.length > HEAD_LINES + TAIL_LINES ? allLines.slice(-TAIL_LINES).join("\n") : "";
    const text =
      `[handle ${id}: ${handle.kind}, ${full.length.toLocaleString()} chars, ${allLines.length.toLocaleString()} lines]\n` +
      `${head}\n${tail ? `…\n${tail}\n` : ""}` +
      `[use peek("${id}", {lines:[from,to]} | {grep:"pattern"}) to read more; do not re-run the command]`;
    return { content: [{ type: "text", text }], details: { arbosHandle: id } };
  });

  // ---- 3. route (main session only: a pi subagent's `route` would hit the main turn's state) ----
  if (SESSION_ROLE === "main") pi.registerTool({
    name: "route",
    label: "Route",
    description: "Required first call of every turn: reply | act | wake | new (see the system prompt).",
    promptSnippet: "route: the required first call of every turn",
    parameters: ROUTE_PARAMS,
    exposure: "model-only",
    executionMode: "sequential",
    async execute(_toolCallId, params): Promise<{ content: { type: "text"; text: string }[]; details: RouteDetails; terminate?: boolean }> {
      const args = params as RouteArgs;
      routeAttempts += 1;
      const res = await hook("/hook/route", { session_id: sessionId, role: SESSION_ROLE, attempt: routeAttempts, args });
      if (res && !res.ok) {
        if (routeAttempts >= 2) {
          routed = true;
          return {
            content: [{ type: "text", text: `Route invalid twice (${res.error ?? "unknown"}). Arbos treats this turn as reply. Write your answer now.` }],
            details: { fallback: "reply", error: res.error },
          };
        }
        throw new Error(`Invalid route: ${res.error ?? "unknown"}. Call route again with corrected arguments.`);
      }
      routed = true;
      const action = args.action;
      if (correcting) {
        // Post-hoc route after a reply that skipped it: record it, do not answer again.
        correcting = false;
        void hook("/hook/route-corrected", { session_id: sessionId, action });
        return { content: [{ type: "text", text: "Route recorded. Do not repeat your answer; end the turn." }], details: { action }, terminate: true };
      }
      switch (action) {
        case "reply":
          if (args.reply && args.reply.trim()) {
            return { content: [{ type: "text", text: "Reply delivered to the user." }], details: { action }, terminate: true };
          }
          return { content: [{ type: "text", text: "Route accepted: reply. Write the reply now." }], details: { action } };
        case "act":
          return {
            content: [{ type: "text", text: `Route accepted: act. Follow your plan with at most ${ACT_MAX_CALLS} tool calls, then summarize.` }],
            details: { action },
          };
        case "wake":
        case "new":
          // No terminate: the model writes the one-sentence acknowledgement itself (demo review item 2).
          return {
            content: [
              {
                type: "text",
                text: `Route accepted: ${action}. Arbos runs the subagent in the background and will hand you its result as a later [Arbos] message. Now write one short sentence to the user saying what you handed off, then end the turn. No list, no JSON, no questions.`,
              },
            ],
            details: { action },
          };
        default: {
          const never: never = action;
          throw new Error(`unknown action ${String(never)}`);
        }
      }
    },
  });

  // ---- 2. ask_user ---------------------------------------------------------------------------
  pi.registerTool({
    name: "ask_user",
    label: "Ask user",
    description: "Ask the user one question and stop; the answer arrives as the next message.",
    promptSnippet: "ask_user: ask the user one question and wait",
    parameters: Type.Object({ question: Type.String({ description: "The question, one or two sentences" }) }),
    exposure: "model-only",
    executionMode: "sequential",
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const res = await hook("/hook/ask_user", { session_id: sessionId, question: params.question });
      if (res?.ok) {
        return { content: [{ type: "text", text: "Question posted. Stop here; the user's answer arrives as the next message." }], details: { question: params.question }, terminate: true };
      }
      if (ctx.hasUI) {
        const answer = await ctx.ui.input("Agent question", params.question);
        return { content: [{ type: "text", text: answer ? `User answered: ${answer}` : "User gave no answer." }], details: { question: params.question, answer } };
      }
      return { content: [{ type: "text", text: "No user is reachable; proceed with your best assumption and say what you assumed." }], details: { question: params.question } };
    },
  });

  // ---- 4c. peek ------------------------------------------------------------------------------
  pi.registerTool({
    name: "peek",
    label: "Peek",
    description: "Read a slice of an offloaded output by handle: lines [from,to] (1-based), chars [from,to], or grep regex; max 200 lines.",
    promptSnippet: "peek: read a slice of a big tool output by handle",
    parameters: Type.Object({
      handle: Type.String(),
      lines: Type.Optional(Type.Array(Type.Integer(), { minItems: 2, maxItems: 2 })),
      chars: Type.Optional(Type.Array(Type.Integer(), { minItems: 2, maxItems: 2 })),
      grep: Type.Optional(Type.String()),
      max: Type.Optional(Type.Integer({ minimum: 1, maximum: 200 })),
    }),
    async execute(_toolCallId, params) {
      const h = handles.get(params.handle);
      let path = h?.path;
      if (!path) {
        const remote = await hook("/hook/handle-path", { handle: params.handle });
        if (remote?.ok && typeof remote.text === "string") path = remote.text;
      }
      if (!path || !existsSync(path)) throw new Error(`unknown handle ${params.handle}`);
      const full = readFileSync(path, "utf8");
      const max = params.max ?? 200;
      let out: string;
      if (params.grep) {
        const re = new RegExp(params.grep);
        out = full
          .split("\n")
          .map((l, i) => [i + 1, l] as const)
          .filter(([, l]) => re.test(l))
          .slice(0, max)
          .map(([n, l]) => `${n}: ${l}`)
          .join("\n");
      } else if (params.chars) {
        out = full.slice(params.chars[0], params.chars[1]);
      } else {
        const [from, to] = params.lines ?? [1, max];
        out = full
          .split("\n")
          .slice(Math.max(0, from - 1), Math.min(to, from - 1 + max))
          .map((l, i) => `${from + i}: ${l}`)
          .join("\n");
      }
      if (out.length > 16_000) out = `${out.slice(0, 16_000)}\n…[truncated; narrow the range]`;
      return { content: [{ type: "text", text: out || "(empty slice)" }], details: { handle: params.handle } };
    },
  });

  // ---- 4d. remember / recall scratchpad (proposals to the write decision, never T1 writes) --
  pi.registerTool({
    name: "remember",
    label: "Remember",
    description: "Save a short named note in this chat's working set (null forgets).",
    parameters: Type.Object({ name: Type.String(), value: Type.Union([Type.String(), Type.Null()]) }),
    async execute(_toolCallId, params) {
      if (params.value === null) scratch.delete(params.name);
      else scratch.set(params.name, params.value);
      pi.appendEntry("arbos-remember", { name: params.name, value: params.value });
      void hook("/hook/remember", { session_id: sessionId, name: params.name, value: params.value });
      return { content: [{ type: "text", text: params.value === null ? `Forgot ${params.name}.` : `Remembered ${params.name}.` }], details: undefined };
    },
  });
  pi.registerTool({
    name: "recall",
    label: "Recall",
    description: "Read a note saved with remember.",
    parameters: Type.Object({ name: Type.String() }),
    async execute(_toolCallId, params) {
      const v = scratch.get(params.name);
      return { content: [{ type: "text", text: v ?? `(no note named ${params.name})` }], details: undefined };
    },
  });

  // Route guard, part 1 (prevention): every main-agent prompt carries a one-line reminder, so a resumed or
  // long session cannot drift into answering without `route` (measured 2/20 skips without it).
  pi.on("input", async (event) => {
    if (SESSION_ROLE !== "main" || event.source === "extension" || event.text.startsWith("/")) return { action: "continue" };
    if (event.text.startsWith(ROUTE_PREFIX)) return { action: "continue" };
    return { action: "transform", text: `${ROUTE_PREFIX}${event.text}`, images: event.images };
  });

  // Route guard, part 2 (correction): a run that still ends without `route` gets one corrective
  // continuation when pi allows one (`canContinue`: the run ended on tool calls, not on plain text);
  // otherwise the miss is logged and agentd records the route as `reply`.
  pi.on("agent_before_settle", async (event) => {
    if (SESSION_ROLE !== "main" || routed) return undefined;
    if (routeCorrections >= 1 || !event.context.canContinue) {
      void hook("/hook/turn-without-route", { session_id: sessionId, corrected: false, can_continue: event.context.canContinue });
      return undefined;
    }
    routeCorrections += 1;
    correcting = true;
    void hook("/hook/turn-without-route", { session_id: sessionId, corrected: true });
    return {
      continue: true,
      entries: [
        {
          type: "custom_message",
          customType: "arbos-route-reminder",
          content:
            "Your previous turn did not start with the required `route` call. Call `route` now with the action that matches what you already did (`reply` if you answered; put nothing in `reply`). Do not repeat your answer.",
          display: false,
        },
      ],
    };
  });

  pi.on("session_compact", async () => {
    // The working set is rebuilt from `handles`/`scratch` on the next before_agent_start,
    // so it survives compaction by construction (design §6.6).
    void hook("/hook/compacted", { session_id: sessionId });
  });

  audit({ kind: "extension_loaded", rules: DENY_RULES.map((r) => r.name) });
}
