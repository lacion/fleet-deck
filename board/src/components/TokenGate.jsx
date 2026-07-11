import React, { useEffect, useRef, useState } from 'react';

// The one failure state for a 401 (v1.7). It replaces the board outright:
// nothing behind it works, and a dead board with a small red toast would just
// invite the human to keep clicking things that cannot happen.
//
// No jargon on purpose — "token", "bearer", "401" are the daemon's words, not
// the user's. What they need to know: this board is on the network, it wants
// the key, and the key was printed next to the link they were told to open.
export default function TokenGate({ attempts, onSubmit }) {
  const [value, setValue] = useState('');
  const inputRef = useRef(null);
  useEffect(() => { inputRef.current?.focus(); }, []);

  const retry = attempts > 1; // they already gave us something and it bounced
  const submit = (e) => {
    e.preventDefault();
    const key = value.trim();
    if (key) onSubmit(key);
  };

  return (
    <div className="fd fd-gate">
      <form className="fd-gatecard" onSubmit={submit}>
        <div className="fd-wordmark">FLEET&nbsp;DECK&nbsp;⚡</div>
        <div className="t1">{retry ? 'THAT KEY DIDN’T WORK' : 'THIS BOARD NEEDS ITS KEY'}</div>
        <div className="t2">
          {retry
            ? 'The board is running, but it didn’t accept that key. Copy the link exactly as the daemon printed it — the key is the part after ?t=.'
            : 'You’re opening the board over the network, so it asks for the key before it shows you anything — this board can start agents and type into their terminals.'}
        </div>
        <div className="hint">
          Open the link the daemon printed when it started:
          <span className="cmd">http://&lt;this-machine&gt;:4711/?t=<span className="c">&lt;key&gt;</span></span>
          …or paste just the key here.
        </div>
        <div className="row">
          <input
            ref={inputRef}
            className="fd-input"
            type="text"
            spellCheck="false"
            autoComplete="off"
            aria-label="Board key"
            placeholder="paste the key"
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
          <button type="submit" className="fd-send" disabled={!value.trim()}>Unlock</button>
        </div>
        <div className="foot">Kept on this device only, so you only do this once per browser.</div>
      </form>
    </div>
  );
}
