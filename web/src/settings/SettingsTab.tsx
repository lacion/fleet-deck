// DL-172: one settings tab: the deck's own settings or the settings of one station. It loads the
// view, keeps the edits, saves only the changed keys and shows what the deck answers.
import { type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { DeckSession, MonitorInfo, SettingsSaved, SettingsView, StationView } from '../gen';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { Icon, RefreshCw } from '../ui/icons';
import { DECK, type SettingsActions, type Target } from './actions';
import { RestartDialog, RestartOverlay, type RestartView, SecretDialog, waitStep } from './dialogs';
import { useElevation } from './elevation';
import { relaySecret, tryRestart, trySave, UNSUPPORTED_TEXT } from './flow';
import { describe } from './http';
import { Loading } from './Loading';
import { NoticeIcon } from './NoticeIcon';
import { OmarchyCard } from './OmarchyCard';
import type { DesktopToolsActions } from './desktopTools';
import { DesktopToolsCard } from './DesktopToolsCard';
import type { OmarchyActions } from './omarchy';
import { asInstances, changedKeys, changesOf, labelOf, sectionId, type Values } from './model';
import { SectionIndex } from './SectionIndex';
import { type SectionSlot, SettingsFields } from './SettingsFields';

export type NoticeItem = {
  readonly id: 'restart' | 'uncertain' | 'changed' | 'error' | 'secret';
  readonly tone: 'info' | 'warn' | 'error';
  readonly text: string;
};

/** The state of a tab at the first render (tests). */
export type InitialTab = {
  readonly view: SettingsView;
  readonly edits?: Values;
  readonly notices?: readonly NoticeItem[];
  readonly errors?: Readonly<Record<string, string>>;
};

type Props = {
  /** The station of this tab; `null`: the deck's own settings. */
  readonly stationId: string | null;
  /** The name for the restart ("Restart Alpha…"). */
  readonly name: string;
  readonly actions: SettingsActions;
  /** The connection to the process is live (the deck: the board; a station: it is reachable). */
  readonly live: boolean;
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
  /** The monitors that the station reports; `undefined` when it reports no desktop. */
  readonly monitors?: readonly MonitorInfo[] | undefined;
  readonly initial?: InitialTab;
  /** The Omarchy calls; the card shows only on a live station tab when this is set. */
  readonly omarchy?: OmarchyActions | undefined;
  /** The desktop tools card (BG-1); same condition as the Omarchy card. */
  readonly desktopTools?: DesktopToolsActions | undefined;
  /** The paired stations (the worker grant stations of the deck tab). */
  readonly stations?: readonly StationView[] | undefined;
  /** The operating system of the station (`traits.os`), for the default of `host`. */
  readonly os?: string | undefined;
  /** The cards of the page by section title (the Tailnet card of the deck's Network section). */
  readonly slots?: Readonly<Record<string, SectionSlot>> | undefined;
  /** The keys that a card shows now: the field list leaves them out (SettingsFields). */
  readonly omit?: readonly string[] | undefined;
};

export function Notice({
  item,
  action,
}: {
  readonly item: NoticeItem;
  readonly action?: ReactNode;
}) {
  return (
    <div
      className="fd-set-notice"
      data-tone={item.tone}
      role={item.tone === 'error' ? 'alert' : 'status'}
    >
      <NoticeIcon tone={item.tone} />
      <span>{item.text}</span>
      {action}
    </div>
  );
}

/**
 * The bar at the bottom of the page while there are changes: the words, then the count as a
 * `.fd-count` pill (§5: a count after a label), then Discard and Save.
 */
export function SaveBar({
  count,
  saving,
  onDiscard,
  onSave,
}: {
  readonly count: number;
  readonly saving: boolean;
  readonly onDiscard: () => void;
  readonly onSave: () => void;
}) {
  return (
    <div className="fd-set-bar" role="region" aria-label="Unsaved changes">
      <span className="fd-set-bar-label">
        Unsaved changes <span className="fd-count">{count}</span>
      </span>
      <div className="fd-set-bar-actions">
        <button type="button" className="fd-ghost" disabled={saving} onClick={onDiscard}>
          Discard
        </button>
        <button type="button" className="fd-primary" disabled={saving} onClick={onSave}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    </div>
  );
}

const withoutNotice = (list: readonly NoticeItem[], ...ids: NoticeItem['id'][]) =>
  list.filter((n) => !ids.includes(n.id));

/** The notices that a saved answer needs. */
export function noticesOfSaved(saved: SettingsSaved): NoticeItem[] {
  const out: NoticeItem[] = [];
  const n = saved.restart_needed.length;
  if (n > 0) {
    out.push({
      id: 'restart',
      tone: 'info',
      text: n === 1 ? '1 setting applies after a restart.' : `${n} settings apply after a restart.`,
    });
  }
  if (saved.durability === 'uncertain') {
    out.push({
      id: 'uncertain',
      tone: 'warn',
      text: 'Saved and applied. The disk did not confirm the write; check the disk.',
    });
  }
  return out;
}

type SecretState = {
  readonly instanceId: string;
  readonly name: string | null;
  readonly busy: boolean;
  readonly error: string | null;
};

export function SettingsTab({
  stationId,
  name,
  actions,
  live,
  list,
  sessions,
  monitors,
  initial,
  omarchy,
  desktopTools,
  stations,
  os,
  slots,
  omit,
}: Props) {
  const target = useMemo<Target>(
    () => (stationId === null ? DECK : { kind: 'station', id: stationId }),
    [stationId],
  );
  const key = stationId ?? 'deck';
  const [view, setView] = useState<SettingsView | null>(initial?.view ?? null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [edits, setEdits] = useState<Values>(initial?.edits ?? {});
  const [errors, setErrors] = useState<Readonly<Record<string, string>>>(initial?.errors ?? {});
  const [notices, setNotices] = useState<readonly NoticeItem[]>(initial?.notices ?? []);
  const [saving, setSaving] = useState(false);
  const [secret, setSecret] = useState<SecretState | null>(null);
  const [restart, setRestart] = useState<RestartView | null>(null);
  const [waiting, setWaiting] = useState<{ since: number; sawDown: boolean } | null>(null);
  const [giveUp, setGiveUp] = useState(false);
  const { ask, node: elevationNode } = useElevation(actions);
  const turn = useRef(0);
  const noticeTop = useRef<HTMLDivElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const noticeCount = notices.length;
  // A notice appears at the top of the tab; the page may be scrolled far down.
  useEffect(() => {
    if (noticeCount > 0) noticeTop.current?.scrollIntoView?.({ block: 'start' });
  }, [noticeCount]);

  const load = useCallback(() => {
    const mine = ++turn.current;
    setLoadError(null);
    actions.view(target).then(
      (v) => {
        if (mine !== turn.current) return;
        setView(v);
        setEdits({});
        setErrors({});
      },
      (e: unknown) => {
        if (mine === turn.current) setLoadError(describe(e));
      },
    );
  }, [actions, target]);
  useEffect(() => {
    if (initial === undefined) load();
    return () => {
      turn.current += 1;
    };
  }, [load, initial]);

  // The wait after a restart: done when the connection is live again after a drop.
  useEffect(() => {
    if (waiting === null) return undefined;
    const tick = () => {
      const step = waitStep(waiting.sawDown, live, Date.now() - waiting.since);
      if (step.done) {
        setWaiting(null);
        setGiveUp(false);
        setNotices((cur) => withoutNotice(cur, 'restart'));
        load();
      } else {
        if (step.sawDown !== waiting.sawDown) setWaiting({ ...waiting, sawDown: step.sawDown });
        setGiveUp(step.giveUp);
      }
    };
    tick();
    const timer = setInterval(tick, 1_000);
    return () => clearInterval(timer);
  }, [waiting, live, load]);

  if (view === null) {
    if (loadError === null) return <Loading text="Loading settings…" />;
    // The cards of the page have their own reads: they stay when the fields cannot load.
    return (
      <div className="fd-set-tab" data-target={key}>
        <div className="fd-set-notice" data-tone="error" role="alert">
          <NoticeIcon tone="error" />
          <span>{loadError}</span>
          <button type="button" className="fd-sm" onClick={load}>
            <Icon of={RefreshCw} size={14} /> Try again
          </button>
        </div>
        {Object.entries(slots ?? {}).map(([title, slot]) =>
          slot.top === undefined ? null : (
            <section key={title} id={sectionId(title)} className="fd-set-sec" aria-label={title}>
              <div className="fd-set-sechead-row">
                <h2 className="fd-set-sechead">{title}</h2>
              </div>
              {slot.top}
            </section>
          ),
        )}
      </div>
    );
  }

  const known = view.fields.map((f) => f.key);
  const changed = changedKeys(view, edits);
  const savedIds = asInstances(view.values['provider_instance']).map((i) => i.id);
  const secretBlocked = changed.includes('provider_instance')
    ? 'Save or discard your changes to the logins before you set a variable.'
    : null;

  const edit = (k: string, value: unknown) => {
    setEdits((cur) => ({ ...cur, [k]: value }));
    setErrors((cur) => {
      if (!(k in cur)) return cur;
      const { [k]: _gone, ...rest } = cur;
      return rest;
    });
  };

  const applySaved = (saved: SettingsSaved, patch?: (v: Values) => Values) => {
    setView((cur) =>
      cur === null ? cur : { ...cur, values: saved.values, revision: saved.revision },
    );
    setEdits(patch === undefined ? {} : patch);
    setErrors({});
    setNotices((cur) => [
      ...withoutNotice(cur, 'restart', 'uncertain', 'changed', 'error'),
      ...noticesOfSaved(saved),
    ]);
  };

  const save = async () => {
    setSaving(true);
    const changes = changesOf(view, edits);
    let out = await trySave(actions, target, changes, view.revision, known);
    if (out.kind === 'elevate') {
      const token = await ask(out.keys.map((k) => labelOf(view.fields, k)));
      if (token === null) {
        setSaving(false);
        return;
      }
      out = await trySave(actions, target, changes, view.revision, known);
    }
    setSaving(false);
    switch (out.kind) {
      case 'saved':
        applySaved(out.saved);
        break;
      case 'elevate':
        setNotices((cur) => [
          ...withoutNotice(cur, 'error'),
          {
            id: 'error',
            tone: 'error',
            text: 'The deck did not accept the confirmation. Try again.',
          },
        ]);
        break;
      case 'changed':
        setNotices((cur) => [
          ...withoutNotice(cur, 'changed', 'error'),
          { id: 'changed', tone: 'warn', text: 'These settings changed since you opened them.' },
        ]);
        break;
      case 'invalid':
        setErrors(Object.fromEntries(out.keys.map((k) => [k, out.text])));
        setNotices((cur) => withoutNotice(cur, 'error'));
        break;
      case 'failed':
        setNotices((cur) => [
          ...withoutNotice(cur, 'error'),
          { id: 'error', tone: 'error', text: out.text },
        ]);
        break;
    }
  };

  const submitSecret = async (
    instanceId: string,
    varName: string,
    access: { read: () => string; clear: () => void },
  ) => {
    setSecret({ instanceId, name: varName, busy: true, error: null });
    const run = () =>
      relaySecret(actions, {
        station: stationId ?? '',
        key: `provider_instance.${instanceId}.env.${varName}`,
        revision: view.revision,
        read: access.read,
        clear: access.clear,
      });
    let out = await run();
    if (out.kind === 'elevate') {
      const token = await ask(['Provider logins']);
      if (token === null) {
        setSecret({ instanceId, name: varName, busy: false, error: null });
        return;
      }
      out = await run();
    }
    switch (out.kind) {
      case 'saved': {
        const saved = out.saved;
        setSecret(null);
        // The other edits stay; the logins show the new state of the file.
        applySaved(saved, (cur) => {
          const { provider_instance: _logins, ...rest } = cur;
          return rest;
        });
        break;
      }
      case 'changed':
        setSecret(null);
        setNotices((cur) => [
          ...withoutNotice(cur, 'changed'),
          { id: 'changed', tone: 'warn', text: 'These settings changed since you opened them.' },
        ]);
        break;
      case 'empty':
        setSecret({ instanceId, name: varName, busy: false, error: 'Type a value.' });
        break;
      case 'elevate':
        setSecret({
          instanceId,
          name: varName,
          busy: false,
          error: 'The confirmation ended. Confirm again, then type the value again.',
        });
        break;
      case 'failed':
        setSecret({ instanceId, name: varName, busy: false, error: out.text });
        break;
    }
  };

  const confirmRestart = async (stop: boolean) => {
    setRestart({ phase: 'busy' });
    let out = await tryRestart(actions, target, stop);
    if (out.kind === 'elevate') {
      const token = await ask(['Restart']);
      if (token === null) {
        setRestart({ phase: 'confirm' });
        return;
      }
      out = await tryRestart(actions, target, stop);
    }
    switch (out.kind) {
      case 'restarting':
        setRestart(null);
        setGiveUp(false);
        setWaiting({ since: Date.now(), sawDown: false });
        break;
      case 'would_stop':
        setRestart({ phase: 'would_stop', items: out.items });
        break;
      case 'unsupported':
        setRestart({ phase: 'failed', text: UNSUPPORTED_TEXT });
        break;
      case 'elevate':
        setRestart({ phase: 'failed', text: 'The confirmation ended. Try again.' });
        break;
      case 'failed':
        setRestart({ phase: 'failed', text: out.text });
        break;
    }
  };

  const reload = () => {
    setNotices((cur) => withoutNotice(cur, 'changed', 'error'));
    load();
  };

  // The station cards sit right after the Desktop section, where their settings are.
  const cards =
    live && omarchy !== undefined && stationId !== null ? (
      <>
        <OmarchyCard station={name} actions={omarchy} ask={ask} />
        {desktopTools === undefined ? null : (
          <DesktopToolsCard station={name} actions={desktopTools} ask={ask} />
        )}
      </>
    ) : null;
  const allSlots: Readonly<Record<string, SectionSlot>> =
    cards === null ? (slots ?? {}) : { ...slots, Desktop: { ...slots?.['Desktop'], after: cards } };

  return (
    <div className="fd-set-tab" data-target={key}>
      <div ref={body} className="fd-set-body">
        <div ref={noticeTop} className="fd-set-notices">
          {notices.map((item) => (
            <Notice
              key={item.id}
              item={item}
              action={
                item.id === 'restart' ? (
                  <button
                    type="button"
                    className="fd-sm"
                    onClick={() => setRestart({ phase: 'confirm' })}
                  >
                    Restart {name}…
                  </button>
                ) : item.id === 'changed' ? (
                  <button type="button" className="fd-sm" onClick={reload}>
                    Reload
                  </button>
                ) : undefined
              }
            />
          ))}
        </div>
        <SettingsFields
          view={view}
          edits={edits}
          errors={errors}
          onEdit={edit}
          stationId={stationId}
          list={list}
          sessions={sessions}
          monitors={monitors}
          savedIds={savedIds}
          secretBlocked={secretBlocked}
          onSecret={(instanceId, varName) =>
            setSecret({ instanceId, name: varName, busy: false, error: null })
          }
          stations={stations}
          os={os}
          slots={allSlots}
          omit={omit}
        />
        {changed.length === 0 ? null : (
          <SaveBar
            count={changed.length}
            saving={saving}
            onDiscard={() => {
              setEdits({});
              setErrors({});
            }}
            onSave={() => void save()}
          />
        )}
      </div>
      <SectionIndex body={body} />
      {secret === null ? null : (
        <SecretDialog
          title={secret.name === null ? 'Add a variable' : `Replace ${secret.name}`}
          name={secret.name}
          busy={secret.busy}
          error={secret.error}
          onCancel={() => setSecret(null)}
          onSubmit={(varName, access) => void submitSecret(secret.instanceId, varName, access)}
        />
      )}
      {restart === null ? null : (
        <RestartDialog
          name={name}
          view={restart}
          onConfirm={(stop) => void confirmRestart(stop)}
          onCancel={() => setRestart(null)}
        />
      )}
      {waiting === null ? null : (
        <RestartOverlay name={name} giveUp={giveUp} onClose={() => setWaiting(null)} />
      )}
      {elevationNode}
    </div>
  );
}
