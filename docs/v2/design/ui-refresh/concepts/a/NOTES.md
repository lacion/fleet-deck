# Direction A "Flight Deck": notes

Built: `styles.css` (all tokens as CSS custom properties; `[data-theme="alt"]` = day deck), `fleet.html`, `session.html`, `new.html`, `phone-home.html`, `phone-session.html`, and `shots/` (6 PNG, from `shoot.mjs a`: 6 "wrote" lines, no page error). Mona Sans (width axis 78 / 115 / 100) and JetBrains Mono load from the Google Fonts link of the direction file. Icons are Lucide 0.469.0.

## Where I did not follow the direction file, and why
- `--ink-3` dark is `#8896A7` (direction: `#7C8899`). The direction value is 3.8:1 on `--strip-hi`; the lighter value is 4.5:1.
- Light mode text colors: green `#0B7A4D`, red `#C42F26`, blue `#1A5FBF`, yellow text `#8A6400`. The fill tokens stay as in the direction file; the text variants reach 4.5:1 on white and on `--bay`.
- Strip callsign cell is 110 px (direction: 120 px). The main column is only about 730 px wide at 1440 (rail 60 + bay 296 + ops 320), and the longest branch (`docs/omarchy-integration`) and the longest focus line must not be cut. Branch text is 11.5 px mono.
- The Needs-you buttons on the fleet home read "Allow", "For session", "Deny"; the full "Allow for this session" does not fit next to the full command. The session page and the phone use the full label.
- Strips in a bay show the station name under the branch (as in the direction drawing) even though the bay header names it. Needs-you sessions are lifted out of their bay into the Needs-you panel, so a bay shows 2 strips for "3 of 12" (the yellow tick is the lifted one). The sidebar follows the same split.
- Session header meta are chips (22 px pills). The inspector also shows the Claude 5-hour window bar and the "signed in" state of Claude and Codex (both from BRIEF.md §4).
- Phone: tool rows and the code copy button have a 44 px touch target (tool rows are 34 px on desktop). The code block scrolls sideways; its right edge fades out.
- The Bypass hazard hint is not drawn: the mock shows "Accept edits" selected, so the hint has no state to appear in. Bypass is red text with a `triangle-alert` icon.
- The `Changes` tab badge says 2 (BRIEF.md) while the inspector lists the one edited file so far (`shim/reaper.rs`).

## Weakest spots
1. Fleet home at 1440 is tight. The three fixed columns leave little room for the strip; every field is near its limit (branch, focus line, "2 subagents" chip). Any longer branch name or title would truncate. A real build needs a narrower ops column or a collapsible bay.
2. Session page: the transcript is longer than the space above the approval card, so the page shows all items only because rows, table and code are tightened (tool rows 34 px, code header 28 px). The inspector has empty space in its lower third, and the composer is one line high, not two.
3. New session is calm but plain: the 160 px task box is mostly empty, and nothing on the page uses the strip idea; it reads as a generic settings form. The sidebar bay repeats the fleet home sessions on every page, which is a lot of repeated yellow on the Needs-you pages.
