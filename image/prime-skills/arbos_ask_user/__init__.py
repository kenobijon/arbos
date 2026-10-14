"""arbos_ask_user: D64 Prime skill. Writes a question file that agentd watches."""

import json
import os
import time
import uuid

ASK_DIR = os.path.join(os.environ.get("ARBOS_STATE_DIR", "/work/.arbos"), "ask")


async def run(question: str) -> str:
    os.makedirs(ASK_DIR, exist_ok=True)
    qid = uuid.uuid4().hex
    payload = {
        "id": qid,
        "session_id": os.environ.get("ARBOS_SESSION_ID", ""),
        "question": question,
        "ts": int(time.time() * 1000),
    }
    tmp = os.path.join(ASK_DIR, f".{qid}.tmp")
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(payload, fh)
    os.replace(tmp, os.path.join(ASK_DIR, f"{qid}.json"))
    return "waiting: the question was posted to the user; stop and wait for the next prompt."
