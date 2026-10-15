/**
 * The `route` tool parameters (design §6.2). Keep byte-stable: it sits in the
 * cached prompt prefix. The Rust validator in arbos-core::route is the source of truth.
 */
import { Type, type Static } from "typebox";

export const ROUTE_PARAMS = Type.Object(
  {
    action: Type.Union([Type.Literal("reply"), Type.Literal("act"), Type.Literal("wake"), Type.Literal("new")]),
    subagent_id: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "wake: which subagent" })),
    name: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "new: short-name" })),
    purpose: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "new: one sentence" })),
    brief: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "new/wake: exact prompt for the subagent" })),
    plan: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "act: <= 3 lines" })),
    harness: Type.Optional(Type.Union([Type.Literal("prime"), Type.Literal("claude_code"), Type.Literal("pi"), Type.Null()])),
    isolate: Type.Optional(Type.Boolean()),
    data_heavy: Type.Optional(Type.Boolean()),
    reply: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "reply: short answer text" })),
  },
  { additionalProperties: false },
);

export type RouteArgs = Static<typeof ROUTE_PARAMS>;
