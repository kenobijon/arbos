"""arbos_ask_user: D64 Prime skill. Writes a question file that agentd watches.

`widget` (optional) is one of the typed shapes from arbos-design.md §10:
  {"type": "choice", "options": [{"id": ..., "label": ...}]}
  {"type": "multi_select", "options": [...]}
  {"type": "text", "placeholder": "...", "multiline": false}
  {"type": "date", "min": "yyyy-mm-dd", "max": "yyyy-mm-dd"}
  {"type": "form", "fields": [{"id", "label", "type": text|number|choice|date|bool, "options"?, "required"?}]}
agentd validates it and falls back to a plain question when it is malformed.
"""

import json
import os
import time
import uuid

ASK_DIR = os.path.join(os.environ.get("ARBOS_STATE_DIR", "/work/.arbos"), "ask")
WIDGET_TYPES = ("choice", "multi_select", "text", "date", "form")


async def run(question: str, widget: dict | None = None) -> str:
    os.makedirs(ASK_DIR, exist_ok=True)
    if widget is not None and widget.get("type") not in WIDGET_TYPES:
        return f"error: widget.type must be one of {', '.join(WIDGET_TYPES)}"
    qid = uuid.uuid4().hex
    payload = {
        "id": qid,
        "session_id": os.environ.get("ARBOS_SESSION_ID", ""),
        "question": question,
        "widget": widget,
        "ts": int(time.time() * 1000),
    }
    tmp = os.path.join(ASK_DIR, f".{qid}.tmp")
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(payload, fh)
    os.replace(tmp, os.path.join(ASK_DIR, f"{qid}.json"))
    return "waiting: the question was posted to the user; stop and wait for the next prompt."
