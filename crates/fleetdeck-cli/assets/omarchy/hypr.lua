-- Fleet Deck (installed by `fleetdeck omarchy install`). Reinstalling rewrites this file.
o.window("^fleetdeck-wall-[A-Za-z0-9_-]+$", { border_size = 0 })
o.window("^fleetdeck-preview-[A-Za-z0-9_-]+$", { float = true, size = { 1280, 800 } })
o.bind("SUPER + CTRL + F", "Fleet Deck panel", "omarchy-shell shell toggle fleetdeck.fleet")
o.bind("SUPER + ALT + W", "Fleet Deck wall", "fleetdeck desktop wall show")
o.bind("SUPER + ALT + N", "Fleet Deck: next session that needs you on the wall", "fleetdeck desktop wall next")
o.bind("SUPER + ALT + M", "Fleet Deck: next wall mode", "fleetdeck desktop wall mode next")
