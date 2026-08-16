import React, { useRef } from 'react';
import { useModal } from '../useModal.ts';
import { HOTKEYS, ORCH_COMMANDS, BOARD_ACTIONS, CONCEPTS, GLOSSARY } from '../helpText.ts';

interface HelpOverlayProps {
  onClose: () => void;
}

// The "?" overlay — everything the board can do, on one screen. Content lives
// in helpText.js so Compose's command chips and this overlay can never drift
// apart. Same modal conventions as Compose: scrim click closes, Esc closes (via
// useBoardHotkeys' Esc chain), ✕ closes, Tab is trapped.
export default function HelpOverlay({ onClose }: HelpOverlayProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  useModal(dialogRef); // full trap + initial focus: this is a plain reading dialog

  return (
    <div className="fd-helpwrap" onClick={onClose}>
      <div
        className="fd-help"
        role="dialog"
        aria-modal="true"
        aria-label="Board help"
        ref={dialogRef}
        onClick={(e) => {
          e.stopPropagation();
        }}
      >
        <div className="fd-helphead">
          <span className="lbl">FLEET DECK — WHAT EVERYTHING DOES</span>
          <span className="fd-spacer" />
          <button type="button" className="fd-x" aria-label="Close help" onClick={onClose}>
            ✕
          </button>
        </div>

        <div className="fd-helpbody">
          {/* the six nouns FIRST — this section is what the first-run
              auto-open (App.jsx) exists to teach */}
          <section>
            <h3>Concepts</h3>
            <dl className="fd-helplist">
              {CONCEPTS.map((c) => (
                <React.Fragment key={c.term}>
                  <dt>{c.term}</dt>
                  <dd>{c.def}</dd>
                </React.Fragment>
              ))}
            </dl>
          </section>

          <section>
            <h3>The board</h3>
            <p className="fd-helpintro">
              Every Claude Code session on this machine reports here. Columns are the daemon’s
              judgement (queued → working → verifying → idle → offline) — cards needing a human
              answer glow amber in the inbox rail on the right.
            </p>
            <dl className="fd-helplist">
              {BOARD_ACTIONS.map((a) => (
                <React.Fragment key={a.name}>
                  <dt>
                    <span className="ico">{a.icon}</span> {a.name}
                  </dt>
                  <dd>{a.does}</dd>
                </React.Fragment>
              ))}
            </dl>
          </section>

          <section>
            <h3>Keyboard</h3>
            <dl className="fd-helplist keys">
              {HOTKEYS.map((h) => (
                <React.Fragment key={h.keys}>
                  <dt>
                    <kbd>{h.keys}</kbd>
                  </dt>
                  <dd>{h.does}</dd>
                </React.Fragment>
              ))}
            </dl>
          </section>

          <section>
            <h3>Commanding the orchestrator</h3>
            <p className="fd-helpintro">
              Compose has two modes. Pick a session, a repo, or ALL and your text is <b>mail</b>,
              delivered at the target’s next turn boundary. Pick <b>ORCHESTRATOR</b> and your text
              is a <b>command</b> the daemon runs immediately:
            </p>
            <dl className="fd-helplist cmds">
              {ORCH_COMMANDS.map((c) => (
                <React.Fragment key={c.syntax}>
                  <dt>
                    <code>{c.syntax}</code>
                  </dt>
                  <dd>{c.does}</dd>
                </React.Fragment>
              ))}
            </dl>
          </section>

          <section>
            <h3>Glossary</h3>
            <dl className="fd-helplist">
              {GLOSSARY.map((g) => (
                <React.Fragment key={g.term}>
                  <dt>{g.term}</dt>
                  <dd>{g.def}</dd>
                </React.Fragment>
              ))}
            </dl>
          </section>
        </div>
      </div>
    </div>
  );
}
