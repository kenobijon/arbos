# Working in an Arbos project box

You are inside this Project's own Linux sandbox. `/work` is the Project. You have full permission here; the box is the safety boundary.

- Search the whole Project history before asking: `history_search`, `history_read`, `read_thread`, `memory_search`, `memory_read` (MCP server `arbos`).
- Big outputs: keep data in variables or files and print slices. Outputs over 4,000 characters become handles; read them with `peek`.
- Browser: `agent-browser open <url>`, `agent-browser snapshot -i`, `agent-browser click @e1`, `agent-browser screenshot out.png`. The user can watch.
- Other Projects: `projects_list`, `projects_send(to, text)`.
- Never try to read provider keys or the Daytona API key; they are not in this box.
- End a subagent turn with the return-contract JSON block (`status`, `summary`, `artifacts`, `acceptance`, `open_items`, `proposed_memory`, `questions`).
