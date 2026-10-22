# Direction B "Graphite": notes

Built: `styles.css` (tokens for dark and `[data-theme="alt"]` light), `fleet.html`, `session.html`, `new.html`, `phone-home.html`, `phone-session.html`, `shots/` (6 PNG). Fonts: Geist and Geist Mono. Icons: Lucide 0.469.0. The status glyphs are one inline SVG sprite per page (working, verifying, needs you, queued, idle, ended, lost). Only the glyphs in the scenario show on a screen. Verified in both modes on all 5 pages (the light mode of the pages other than fleet was rendered once with a temporary copy, then removed).

## Where I did not follow the direction file, and why
- `--ink-3` dark is `#888893` (file: `#7D7D88`). The file value is 4.1:1 on `--raised` and 4.3:1 on `--side`. The new value is 4.7:1 or better.
- Light mode only: the amber fill is 1.75:1 on white, so the needs-you glyph and the approval border get a `#C27B00` edge (3.4:1) and the Allow button a faint inner edge. Amber text uses `#9A5B00` as the file says.
- Needs-you rows are 60 px and two lines (kind, tool, age, callsign) instead of 44 px: the brief asks for the request kind and its age.
- New session: the 6 pills are regrouped in 3 labeled rows (Agent, Station, Where). The station is 3 radio pills with online dot and free slots (the brief asks for both). The permission menu opens upward over the empty task area, so it hides no field.
- Row cells show a file name, not a full path (`cart.ts`); long commands truncate. The full text is in the session view.
- Added: a 36 px key-hint bar at the bottom of the fleet main column; the Inbox nav item and the open "More" list (both from the direction file).
- Invented small values (not in the brief): tool durations, "Started 26 min ago", per-file "M" marks in Files changed.
- The Fleet tab is active on the phone home. The Sessions tab has no screen in this mock-up.

## The 3 weakest spots
1. Fleet home: with only 7 sessions about 160 px of canvas stays empty under the last group (and in the right panel). The "now" column is tight: the PR chip and "2 subagents" only fit because the tool target is cut to a file name.
2. The code block has no syntax color (graphite rule: only `--signal` draws the eye), so it reads flat. In light mode the one signal color is quieter, and the amber needs the darker edge to hold up.
3. The session fits one screen only with tight vertical spacing (30 px tool rows, 20 px code lines, a 1-line composer). The phone session shows the end of the transcript only, with a faded code block at the top, and 3 stacked 44 px buttons take almost half of the screen.
