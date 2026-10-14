/**
 * The `route` tool parameters (design §6.2). Keep byte-stable: it sits in the
 * cached prompt prefix. The Rust validator in arbos-core::route is the source of truth.
 */
import { Type, type Static } from "typebox";

export const ROUTE_PARAMS = Type.Object(
  {
    action: Type.Union([Type.Literal("reply"), Type.Literal("act"), Type.Literal("wake"), Type.Literal("new")], {
      description: "reply: answer now. act: do it yourself in <=8 tool calls / 60s. wake: continue an existing subagent. new: start a subagent.",
    }),
    subagent_id: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "required for wake" })),
    name: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "short-name-if-new, e.g. rail-layout" })),
    purpose: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "one sentence if new" })),
    brief: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "the exact prompt to send to the subagent, if wake or new" })),
    plan: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "<= 3 lines: the tool calls you expect to make, if act" })),
    harness: Type.Optional(Type.Union([Type.Literal("prime"), Type.Literal("claude_code"), Type.Literal("pi"), Type.Null()])),
    isolate: Type.Optional(Type.Boolean({ default: false })),
    data_heavy: Type.Optional(Type.Boolean({ default: false })),
    reply: Type.Optional(Type.Union([Type.String(), Type.Null()], { description: "the text to stream, if reply" })),
  },
  { additionalProperties: false },
);

export type RouteArgs = Static<typeof ROUTE_PARAMS>;
