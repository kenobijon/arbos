/**
 * `ask_user` widget schema (design §10 "question widgets"). Mirrors
 * `arbos_core::widget::WIDGET_SCHEMA_JSON`; agentd validates again and falls
 * back to a plain question on anything it does not understand.
 */

import { Type, type Static } from "typebox";

const OPTION = Type.Object({ id: Type.String(), label: Type.String() });

export const WIDGET_PARAM = Type.Object(
  {
    type: Type.Union([
      Type.Literal("choice"),
      Type.Literal("multi_select"),
      Type.Literal("text"),
      Type.Literal("date"),
      Type.Literal("form"),
    ]),
    options: Type.Optional(Type.Array(OPTION, { description: "choice | multi_select" })),
    placeholder: Type.Optional(Type.String()),
    multiline: Type.Optional(Type.Boolean()),
    min: Type.Optional(Type.String({ description: "date: yyyy-mm-dd" })),
    max: Type.Optional(Type.String()),
    fields: Type.Optional(
      Type.Array(
        Type.Object({
          id: Type.String(),
          label: Type.String(),
          type: Type.Union([
            Type.Literal("text"),
            Type.Literal("number"),
            Type.Literal("choice"),
            Type.Literal("date"),
            Type.Literal("bool"),
          ]),
          options: Type.Optional(Type.Array(OPTION)),
          required: Type.Optional(Type.Boolean()),
        }),
        { description: "form: <= 8 fields" },
      ),
    ),
  },
  { description: "Optional typed widget. Omit for a plain text question." },
);

export type WidgetArg = Static<typeof WIDGET_PARAM>;

/** One-line rendering for the local pi TUI fallback (no agentd). */
export function describeWidget(question: string, widget: WidgetArg | null): string {
  if (!widget) return question;
  switch (widget.type) {
    case "choice":
    case "multi_select": {
      const opts = (widget.options ?? []).map((o) => `${o.id}=${o.label}`).join(", ");
      return `${question} [${widget.type}: ${opts}]`;
    }
    case "text":
      return widget.placeholder ? `${question} (${widget.placeholder})` : question;
    case "date":
      return `${question} [date${widget.min ? ` >= ${widget.min}` : ""}${widget.max ? ` <= ${widget.max}` : ""}]`;
    case "form": {
      const fields = (widget.fields ?? []).map((f) => `${f.id}:${f.type}`).join(", ");
      return `${question} [form: ${fields}]`;
    }
    default: {
      const never: never = widget.type;
      return `${question} ${String(never)}`;
    }
  }
}
