import { useState } from 'react';
import { stationName } from '../board/fleet';
import { providerLabel } from '../board/lanes';
import type { DeckSession, OpenRequest, StationView } from '../gen';
import type { Freshness, Timeline } from '../runtime/state';
import type { InputKind, Outcome, SessionActions } from './commands';
import { QuestionAnswer } from './QuestionAnswer';
import { questionForm, type Reply } from './question';
import { FilesPanel } from './FilesPanel';
import { RenameForm } from './RenameForm';
import { buildTimeline, type Item, subagentLabel } from './timeline';
import { RevertStatus, TurnsPanel } from './TurnsPanel';
import { finalizedTurns } from './workspace';

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly timeline: Timeline | undefined;
  readonly freshness: Freshness;
  readonly actions: SessionActions | undefined;
  readonly onBack: () => void;
};

/** One session: approvals first, then controls, the composer and the timeline tree. */
export function SessionView({ session, stations, timeline, freshness, actions, onBack }: Props) {
  const s = session.summary;
  const caps = s.capabilities;
  const live = freshness === 'live' && session.status !== 'unreachable';
  const ended = s.state === 'exited' || s.state === 'lost';
  const drive = live && !ended && actions?.canDrive === true;
  const working = s.current_run !== null;
  const tree = buildTimeline(timeline?.events ?? []);
  const turns = finalizedTurns(timeline?.events ?? []);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [busy, setBusy] = useState(false);
  const act = (call: () => Promise<Outcome>) => {
    setBusy(true);
    void call()
      .then(setOutcome)
      .finally(() => setBusy(false));
  };
  return (
    <main className="fd-session" aria-label="Session">
      <button type="button" onClick={onBack}>
        Back to fleet
      </button>
      <h2>{s.title ?? s.session_id}</h2>
      <RenameForm
        title={s.title}
        enabled={live && actions?.canDrive === true && !busy}
        onRename={(t) => actions && act(() => actions.rename(t))}
      />
      <p className="fd-card-line">
        {stationName(session.station_id, stations)} · {providerLabel(s.provider)} ·{' '}
        <span data-status={session.status}>{session.status.replaceAll('_', ' ')}</span>
        {s.mode ? ` · mode ${s.mode}` : ''}
      </p>
      {!live ? (
        <p className="fd-note" role="status">
          Controls are off until the board and the station are live.
        </p>
      ) : null}
      <Requests
        requests={s.open_requests}
        answerable={caps?.approvals === true}
        enabled={live && !ended && actions?.canApprove === true}
        busy={busy}
        onAnswer={(r, o, reply) => actions && act(() => actions.answer(r, o, reply))}
      />
      <section className="fd-controls" aria-label="Session controls">
        {caps?.interrupt ? (
          <button
            type="button"
            disabled={!drive || !working || busy}
            onClick={() => actions && act(() => actions.interrupt())}
          >
            Interrupt
          </button>
        ) : null}
        {caps?.set_mode && caps.modes.length > 0 ? (
          <ModeLadder
            modes={caps.modes}
            current={s.mode}
            enabled={drive && !busy}
            onSet={(m) => actions && act(() => actions.setMode(m))}
          />
        ) : null}
        {caps?.set_model ? (
          <ModelPicker
            current={s.model}
            enabled={drive && !busy}
            onSet={(m) => actions && act(() => actions.setModel(m))}
          />
        ) : null}
      </section>
      <Composer
        enabled={drive && !busy}
        working={working}
        steer={caps?.steer === true}
        nativeQueue={caps?.native_follow_up === true}
        onSend={(kind, text) => actions && act(() => actions.send(kind, text))}
      />
      {outcome ? (
        <p role={outcome.ok ? 'status' : 'alert'} className="fd-outcome">
          {outcome.text}
        </p>
      ) : null}
      <RevertStatus
        summary={s}
        turns={turns}
        actions={actions}
        drive={drive}
        live={live}
        busy={busy}
        act={act}
      />
      <TurnsPanel
        summary={s}
        turns={turns}
        actions={actions}
        drive={drive}
        live={live}
        busy={busy}
        act={act}
      />
      <FilesPanel files={live && actions?.canDrive === true ? (actions.files ?? null) : null} />
      <section className="fd-timeline" aria-label="Timeline">
        <h3>
          Timeline · subagents running: <span data-running-subagents>{tree.running}</span>
        </h3>
        {timeline?.error ? <p role="alert">{timeline.error}</p> : null}
        {timeline?.ready ? null : <p className="fd-note">Loading the timeline…</p>}
        <Items items={tree.items} />
      </section>
    </main>
  );
}

function Requests(props: {
  requests: readonly OpenRequest[];
  answerable: boolean;
  enabled: boolean;
  busy: boolean;
  onAnswer: (request: string, option: string, reply: Reply | null) => void;
}) {
  if (props.requests.length === 0) return null;
  return (
    <section className="fd-requests" aria-label="Needs you">
      <h3>Needs you</h3>
      <ul>
        {props.requests.map((r) => {
          const form = props.answerable ? questionForm(r) : null;
          return (
            <li key={r.request_id} data-request={r.request_id}>
              <strong>{r.title}</strong>
              <pre className="fd-details">{r.details}</pre>
              {/* Without the approvals capability the provider answers under its
                own policy: the board shows the request but no controls (§6.2). */}
              {/* A question gets an answer form (DL-068). */}
              {form ? (
                <QuestionAnswer
                  request={r}
                  form={form}
                  enabled={props.enabled}
                  busy={props.busy}
                  onAnswer={(o, reply) => props.onAnswer(r.request_id, o, reply)}
                />
              ) : props.answerable ? (
                <div className="fd-options">
                  {r.options.map((o) => (
                    <button
                      key={o.option_id}
                      type="button"
                      disabled={!props.enabled || props.busy}
                      onClick={() => props.onAnswer(r.request_id, o.option_id, null)}
                    >
                      {o.label}
                    </button>
                  ))}
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>
      {!props.answerable ? (
        <p className="fd-note">This provider has no approval controls.</p>
      ) : props.enabled ? null : (
        <p className="fd-note">Answering needs the approve scope.</p>
      )}
    </section>
  );
}

function ModeLadder(props: {
  modes: readonly string[];
  current: string | null;
  enabled: boolean;
  onSet: (mode: string) => void;
}) {
  // The capability lists the modes from the most restrictive (L8).
  return (
    <fieldset className="fd-modes">
      <legend>Mode (most restrictive first)</legend>
      {props.modes.map((m) => (
        <button
          key={m}
          type="button"
          aria-pressed={m === props.current}
          disabled={!props.enabled || m === props.current}
          onClick={() => props.onSet(m)}
        >
          {m}
        </button>
      ))}
    </fieldset>
  );
}

function ModelPicker(props: {
  current: string | null;
  enabled: boolean;
  onSet: (model: string) => void;
}) {
  // Providers accept model ids and aliases that the board cannot list, so the
  // picker takes free text (DL-060).
  const [model, setModel] = useState('');
  const value = model.trim();
  return (
    <form
      className="fd-options"
      aria-label="Model"
      onSubmit={(event) => {
        event.preventDefault();
        if (value === '') return;
        setModel('');
        props.onSet(value);
      }}
    >
      <label htmlFor="fd-model-id">Model (now {props.current ?? 'unknown'})</label>
      <input
        id="fd-model-id"
        value={model}
        disabled={!props.enabled}
        onChange={(event) => setModel(event.target.value)}
        placeholder="Model id or alias"
      />
      <button type="submit" disabled={!props.enabled || value === ''}>
        Set model
      </button>
    </form>
  );
}

function Composer(props: {
  enabled: boolean;
  working: boolean;
  steer: boolean;
  nativeQueue: boolean;
  onSend: (kind: InputKind, text: string) => void;
}) {
  const [text, setText] = useState('');
  const send = (kind: InputKind) => {
    const value = text.trim();
    if (value === '') return;
    setText('');
    props.onSend(kind, value);
  };
  const empty = text.trim() === '';
  return (
    <form
      className="fd-composer"
      aria-label="Composer"
      onSubmit={(event) => {
        event.preventDefault();
        send(props.working ? 'follow_up' : 'prompt');
      }}
    >
      <label htmlFor="fd-composer-text">Message</label>
      <textarea
        id="fd-composer-text"
        rows={3}
        value={text}
        disabled={!props.enabled}
        onChange={(event) => setText(event.target.value)}
      />
      <div className="fd-options">
        {props.working ? (
          <>
            {props.steer ? (
              <button
                type="button"
                disabled={!props.enabled || empty}
                onClick={() => send('steer')}
              >
                Steer now
              </button>
            ) : null}
            <button type="submit" disabled={!props.enabled || empty}>
              Queue follow-up
            </button>
            {props.nativeQueue ? null : (
              <p className="fd-note">The station holds a follow-up until the turn ends.</p>
            )}
          </>
        ) : (
          <button type="submit" disabled={!props.enabled || empty}>
            Send
          </button>
        )}
      </div>
    </form>
  );
}

function Items({ items }: { items: readonly Item[] }) {
  return (
    <ol className="fd-items">
      {items.map((item) => (
        <li key={item.key} data-item={item.type}>
          <ItemView item={item} />
        </li>
      ))}
    </ol>
  );
}

function ItemView({ item }: { item: Item }) {
  switch (item.type) {
    case 'run':
      return (
        <p className="fd-run">
          <strong>{item.origin === 'provider' ? 'Provider turn' : 'You'}:</strong> {item.input}
        </p>
      );
    case 'message':
      return <p className="fd-message">{item.text}</p>;
    case 'tool':
      return (
        <div className="fd-tool" data-tool-status={item.status}>
          <span>
            {item.title} · {item.input} · {item.status}
          </span>
          {item.output ? <pre className="fd-details">{item.output}</pre> : null}
        </div>
      );
    case 'subagent': {
      const label = subagentLabel(item.status);
      return (
        <details className="fd-subagent" data-subagent={item.node_id} open>
          <summary>
            Subagent: {item.input || item.title} · <span data-subagent-status>{label}</span> ·{' '}
            {item.items.length} entries
          </summary>
          {item.items.length === 0 ? (
            <p className="fd-note">No events from this subagent.</p>
          ) : (
            <Items items={item.items} />
          )}
        </details>
      );
    }
    case 'approval':
      return (
        <p className="fd-approval" data-approval={item.state}>
          Approval: {item.title} · {item.outcome ?? 'waiting for an answer'}
        </p>
      );
    case 'turn':
      return <p className="fd-note">Turn {item.status}.</p>;
    case 'notice':
      return item.tone === 'error' ? (
        <p className="fd-notice-error">{item.text}</p>
      ) : (
        <p className="fd-note">{item.text}</p>
      );
  }
}
