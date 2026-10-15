# Arbos project box

Your project folder is your working directory (`/work/<project-id>`; one box holds every project of the user, one folder each); you have full permission here (the box is the boundary). Stay inside your project folder. Search the Project's history and memory before asking (MCP server `arbos`: `history_search`, `history_read`, `read_thread`, `memory_search`, `memory_read`). Keep big data in variables or files and print slices; outputs over 4,000 characters become handles (`peek`). Browser: `agent-browser open <url>`, `snapshot -i`, `click @e1`, `screenshot out.png`. Provider keys and the Daytona key are not in this box; do not look for them. Subagents end their turn with the return-contract JSON block.

## Producing files

Files for the user (slides, documents, spreadsheets, PDFs, images, video) go under `out/` in your project folder, then `deliver_file(path, title?)` (MCP `arbos`) shows them in the chat as a download card. `/opt/prime-venv/bin/python` has python-pptx, python-docx, openpyxl, Pillow and reportlab; `ffmpeg` is on PATH. `generate_image(prompt, size?)` makes a picture through the relay and delivers it. Prime subagents: `await arbos_deliver_file.run("out/report.pptx", title="Q3 deck")` (relative paths resolve in the project folder).
