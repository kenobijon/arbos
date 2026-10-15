# DMG window layout

`dmg.DS_Store` is the Finder window layout of the release DMG (660×400 window,
dark `.background/background.png`, 128 px icons, `Arbos.app` at 165,190 and
`Applications` at 495,190). It is copied into the DMG source folder by
`scripts/bundle-mac.sh`, so the build machine needs no Finder session or Apple
Events (the release runner is a launchd service without either).

It was generated once with the Python `ds_store` and `mac_alias` libraries
(`pip install ds_store mac_alias`), writing `bwsp` (window bounds), `icvp`
(icon view: background image alias, icon size, grid) and `Iloc` entries. The
background alias points at `/Volumes/Arbos/.background/background.png`, so the
volume name must stay `Arbos`. Regenerate with the same recipe when the window
size, icon positions or background file name change; nothing else depends on it.
