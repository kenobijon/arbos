---
name: arbos_ask_user
description: Ask the user one question (optionally with a typed widget) and pause this subagent until the answer arrives as the next prompt.
---

Call `await arbos_ask_user.run("your question")`. It writes `/work/.arbos/ask/<id>.json`; agentd raises the S8 wait state and the answer arrives as your next prompt. Stop working after calling it.

For a click-to-answer card pass a `widget`:

```python
await arbos_ask_user.run("Which branch?", {"type": "choice", "options": [{"id": "main", "label": "main"}, {"id": "dev", "label": "dev"}]})
await arbos_ask_user.run("Pick the files to keep", {"type": "multi_select", "options": [...]})
await arbos_ask_user.run("Commit message?", {"type": "text", "placeholder": "feat: …", "multiline": True})
await arbos_ask_user.run("Ship date?", {"type": "date", "min": "2026-11-01"})
await arbos_ask_user.run("Release details", {"type": "form", "fields": [
    {"id": "version", "label": "Version", "type": "text", "required": True},
    {"id": "channel", "label": "Channel", "type": "choice", "options": [{"id": "beta", "label": "Beta"}, {"id": "stable", "label": "Stable"}]},
    {"id": "notify", "label": "Notify users", "type": "bool"},
]})
```

The answer comes back rendered as text (`Answer: Beta`, or a bullet list for forms).
