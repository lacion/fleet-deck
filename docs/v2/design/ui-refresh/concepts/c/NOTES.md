# Direction C "Signal": notes

Built: `styles.css` (tokens for light and `[data-theme="alt"]` dark), `fleet.html`, `session.html`, `new.html`,
`phone-home.html`, `phone-session.html`, and 6 PNG files in `shots/`. Render: `node ../shoot.mjs c`.
Checked with Playwright in both modes: no page error, no desktop scrollbar, fonts loaded, text contrast >= 4.5:1.

## Where I did not follow the direction file exactly

- `--ink-3` is `#5E6A7E` (file: `#687387`). The file value is 4.39:1 on `--canvas`, below the 4.5:1 floor.
- Text colors `--go-text` `#0B7D3E` and `--stop-text` `#C5292C` (light mode) are darker than `--go` and `--stop`. The fill colors are as in the file.
  `--sun-text` `#8F5A00` is used only for the "waiting" icon.
- Needs-you tickets in the lanes repeat the request in the hero (Allow/Deny again). The file asks for both. The hero buttons have the key hints, the ticket buttons do not.
- The session transcript is anchored to the bottom of the chat column and cropped at the top (the column scrolls inside). The user message, "Thinking" and the Read and Search rows are above the crop. All 11 items are in the HTML.
- The phone session crops the transcript the same way. The phone home fades the list under the floating tab bar with a mask (an opacity fade, not a color wash).
- The theme switch in the top bar and the "more" menu items are static (no menu opens). The ticket "now" line for a question shows the question text.
- Station cards on the new form show free slots (9 free, 10 free, 10 free) and the used count, because the scenario gives "3 of 12" as used.
- Batch is in the Title row (switch off). Bypass shows the hazard icon and `--stop` text only; the hint text for a selected Bypass is not shown (not selected).
- JetBrains Mono ligatures are off, so `->` and `::` stay literal.
- The Mail & tasks detail is only in the "Mail" pill (tooltip) and the "1 open task" chip on `kite-x81d`; the mac-m4 lane is not used for it, because a lane means a station.

## Weakest spots

1. Fleet home: the lanes are unequal (Lighting has 3 tickets, mac-m4 has 1 and an ended row). The lower right is empty. A real board with more sessions fixes this; with this scenario it looks lopsided.
2. Session: the transcript crop hides the user message and the first tool rows. The reader sees the answer and the approval, not the question. A taller chat area would need a smaller header or a collapsed changes column.
3. Needs-you is shown twice on the desktop (hero card and ticket) and the phone repeats the session in the list. It is correct by the file, but the duplicate Allow/Deny buttons add noise and could confuse which one is "the" action.
