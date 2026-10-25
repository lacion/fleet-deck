// The Fleet Deck mark (DESIGN-SYSTEM.md §2.6, DL-461): one path on a 24-unit box, even-odd fill.
// Every brand file comes from this constant: ui/Mark.tsx, tools/brand/render.mjs (favicon, app icons,
// README art, social preview) and the Omarchy bar widget (assets/omarchy/plugin/Widget.qml).
// Concept C, "Formation": five rounded squares in a V, the deck in the lead (owner decision, B16).
export const MARK_PATH =
  'M10.5 2H13.5A2 2 0 0 1 15.5 4V7A2 2 0 0 1 13.5 9H10.5A2 2 0 0 1 8.5 7V4A2 2 0 0 1 10.5 2ZM5.6 10H7.9A1.6 1.6 0 0 1 9.5 11.6V13.9A1.6 1.6 0 0 1 7.9 15.5H5.6A1.6 1.6 0 0 1 4 13.9V11.6A1.6 1.6 0 0 1 5.6 10ZM16.1 10H18.4A1.6 1.6 0 0 1 20 11.6V13.9A1.6 1.6 0 0 1 18.4 15.5H16.1A1.6 1.6 0 0 1 14.5 13.9V11.6A1.6 1.6 0 0 1 16.1 10ZM2 17H4A1.5 1.5 0 0 1 5.5 18.5V20.5A1.5 1.5 0 0 1 4 22H2A1.5 1.5 0 0 1 0.5 20.5V18.5A1.5 1.5 0 0 1 2 17ZM20 17H22A1.5 1.5 0 0 1 23.5 18.5V20.5A1.5 1.5 0 0 1 22 22H20A1.5 1.5 0 0 1 18.5 20.5V18.5A1.5 1.5 0 0 1 20 17Z';

/** The box of MARK_PATH. */
export const MARK_BOX = 24;
