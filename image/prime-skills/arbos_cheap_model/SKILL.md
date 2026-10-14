---
name: arbos_cheap_model
description: Run a cheap model (the Project's talking model) over text through the relay proxy, for map-reduce over big data.
---

`await arbos_cheap_model.run(instruction, text)` returns the model's text. Chunk big inputs yourself (≤ 60,000 chars per call).
