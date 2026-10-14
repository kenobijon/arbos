---
name: arbos_ask_user
description: Ask the user one question and pause this subagent until the answer arrives as the next prompt.
---

Call `await arbos_ask_user.run("your question")`. It writes `/work/.arbos/ask/<id>.json`; agentd raises the S8 wait state and the answer arrives as your next prompt. Stop working after calling it.
