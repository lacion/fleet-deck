// R3.2 (DL-164, DL-171): one form for "New project" and "Edit".
import { useEffect, useState } from 'react';
import type {
  AccountsList,
  AccountView,
  Containers,
  DeckSession,
  ForgeKind,
  StationView,
  TrackerKind,
} from '../gen';
import { FolderPicker, type ListWorkspaces } from '../ui/FolderPicker';
import { Icon, Plus, X } from '../ui/icons';
import { Field, FormActions, FormGrid, fieldError } from '../ui/page';
import {
  draftProblem,
  FORGE_HOST,
  MAX_TRACKERS,
  type ProjectDraft,
  systemLabel,
  type TrackerDraft,
} from './model';
import { ProjectGitSettings } from './ProjectGitSettings';

type Props = {
  readonly stations: readonly StationView[];
  readonly draft: ProjectDraft;
  readonly onChange: (next: ProjectDraft) => void;
  readonly editing: boolean;
  /** The id of the edited project; the Git sync section needs it (FG-1..FG-4). */
  readonly projectId?: string | undefined;
  readonly saving: boolean;
  readonly error: string | null;
  /** The accounts of the project's station (`GET .../accounts`); `null` until they load or when they cannot. */
  readonly accounts: AccountsList | null;
  /** Reads the Jira projects or the Linear teams of an account of the station; absent: the key is typed. */
  readonly loadContainers?:
    | ((station: string, accountId: string) => Promise<Containers>)
    | undefined;
  readonly onSave: () => void;
  readonly onCancel: () => void;
  /** Reads a folder of the station (the folder picker); absent: the folder is typed only. */
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
  /** The fields that show their error at the first render (tests). */
  readonly initialTried?: boolean;
};

const KINDS: readonly TrackerKind[] = ['jira', 'linear'];

/** A draft that passes every rule: one field of a draft is checked on this base. */
const SOUND: ProjectDraft = {
  name: 'x',
  station: 's',
  path: '/p',
  forge: 'none',
  host: '',
  repo: '',
  trackers: [],
  promptTemplate: '',
  reviewPrompt: '',
};

type FieldName = 'name' | 'station' | 'path' | 'repo' | 'trackers';

/** The rule of one field of the draft, or `null`. */
function problemOf(field: FieldName, d: ProjectDraft): string | null {
  switch (field) {
    case 'name':
      return draftProblem({ ...SOUND, name: d.name });
    case 'station':
      return draftProblem({ ...SOUND, station: d.station });
    case 'path':
      return draftProblem({ ...SOUND, path: d.path });
    case 'repo':
      return draftProblem({ ...SOUND, forge: d.forge, repo: d.repo });
    case 'trackers':
      return draftProblem({ ...SOUND, trackers: d.trackers });
  }
}

/** The host after a forge change: the default of the new forge while it is empty or the other default. */
export function hostAfterForge(forge: ProjectDraft['forge'], host: string): string {
  if (forge === 'none') return '';
  const other: ForgeKind = forge === 'github' ? 'gitlab' : 'github';
  return host === '' || host === FORGE_HOST[other] ? FORGE_HOST[forge] : host;
}

/** The label of a tracker account: `acme.atlassian.net (jira-main)`. */
const accountLabel = (a: AccountView): string =>
  `${a.site ?? systemLabel(a.kind)} (${a.account_id})`;

/** The accounts that a tracker row can use: Jira or Linear, by OAuth, by token or from the config file. */
export function trackerAccounts(
  accounts: AccountsList | null,
  kind: TrackerKind,
): readonly AccountView[] {
  return (accounts?.accounts ?? []).filter(
    (a) =>
      a.kind === kind && (a.method === 'oauth' || a.method === 'token' || a.method === 'config'),
  );
}

/** The user of the login of a forge on a host, or `null` when the station has none. */
export function forgeUser(
  accounts: AccountsList | null,
  forge: ForgeKind,
  host: string,
): string | null {
  const found = (accounts?.accounts ?? []).find(
    (a) =>
      a.kind === forge &&
      (a.method === 'cli' || a.method === 'token') &&
      a.status === 'ok' &&
      (a.site === undefined || a.site === (host === '' ? FORGE_HOST[forge] : host)),
  );
  return found === undefined ? null : (found.user ?? '');
}

/** The key of a tracker row: a list of the account's projects or teams, else a text input. */
export function ContainerPicker({
  kind,
  value,
  station,
  accountId,
  load,
  onChange,
  initial = null,
}: {
  readonly kind: TrackerKind;
  readonly value: string;
  readonly station: string;
  readonly accountId: string;
  readonly load: ((station: string, accountId: string) => Promise<Containers>) | undefined;
  readonly onChange: (key: string) => void;
  /** The list at the first render (tests). */
  readonly initial?: Containers | null;
}) {
  const [found, setFound] = useState<Containers | null>(initial);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (load === undefined || accountId === '' || initial !== null) return undefined;
    let live = true;
    setFound(null);
    setFailed(false);
    load(station, accountId).then(
      (v) => live && setFound(v),
      () => live && setFailed(true),
    );
    return () => {
      live = false;
    };
  }, [load, station, accountId, initial]);
  if (found === null || failed) {
    return (
      <Field
        label={kind === 'linear' ? 'Team key' : 'Project key'}
        hint={failed ? 'The list did not load. Type the key.' : undefined}
      >
        <input
          value={value}
          autoComplete="off"
          placeholder={kind === 'linear' ? 'ENG' : 'PROJ'}
          onChange={(e) => onChange(e.target.value)}
        />
      </Field>
    );
  }
  const listed = found.containers.some((c) => c.key === value);
  return (
    <Field label={kind === 'linear' ? 'Linear team' : 'Jira project'}>
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">Choose one</option>
        {listed || value === '' ? null : <option value={value}>{value} (not listed)</option>}
        {found.containers.map((c) => (
          <option key={c.key} value={c.key}>
            {c.name} ({c.key})
          </option>
        ))}
      </select>
    </Field>
  );
}

export function ProjectForm({
  stations,
  draft,
  onChange,
  editing,
  projectId,
  saving,
  error,
  accounts,
  loadContainers,
  onSave,
  onCancel,
  list,
  sessions,
  initialTried = false,
}: Props) {
  const [touched, setTouched] = useState<ReadonlySet<FieldName>>(new Set());
  const [tried, setTried] = useState(initialTried);
  const touch = (field: FieldName) => setTouched((cur) => new Set(cur).add(field));
  const shown = (field: FieldName) =>
    fieldError(problemOf(field, draft), touched.has(field), tried);
  const problem = draftProblem(draft);
  const set = (patch: Partial<ProjectDraft>) => onChange({ ...draft, ...patch });
  const setTracker = (i: number, patch: Partial<TrackerDraft>) =>
    set({ trackers: draft.trackers.map((t, at) => (at === i ? { ...t, ...patch } : t)) });
  const loaded = accounts !== null;
  const anyTracker =
    trackerAccounts(accounts, 'jira').length + trackerAccounts(accounts, 'linear').length;
  const accountsHref = `#/settings/accounts?station=${encodeURIComponent(draft.station)}`;
  const forgeUserText =
    draft.forge === 'none' ? null : forgeUser(accounts, draft.forge, draft.host.trim());
  const title = editing ? 'Edit project' : 'New project';
  return (
    <div className="fd-panel fd-formpanel">
      <form
        aria-label={title}
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          if (saving) return;
          if (problem === null) onSave();
          else setTried(true);
        }}
      >
        <h2 className="fd-form-title">{title}</h2>
        <FormGrid>
          <Field label="Name" error={shown('name')} onBlur={() => touch('name')}>
            <input
              value={draft.name}
              maxLength={80}
              autoComplete="off"
              onChange={(e) => set({ name: e.target.value })}
            />
          </Field>
          <Field label="Station" error={shown('station')} onBlur={() => touch('station')}>
            <select value={draft.station} onChange={(e) => set({ station: e.target.value })}>
              {stations.map((s) => (
                <option key={s.station_id} value={s.station_id}>
                  {s.name}
                </option>
              ))}
            </select>
          </Field>
          <Field
            label="Folder (a repository on the station)"
            span={2}
            group
            error={shown('path')}
            onBlur={() => touch('path')}
          >
            <FolderPicker
              stationId={draft.station}
              value={draft.path}
              onChange={(path) => set({ path })}
              list={list}
              sessions={sessions}
              label="Folder"
            />
          </Field>
        </FormGrid>
        <div className="fd-formsec" role="group" aria-label="Forge">
          <h3 className="fd-label">Forge</h3>
          <FormGrid>
            <Field label="Forge">
              <select
                value={draft.forge}
                onChange={(e) => {
                  const forge = e.target.value as 'none' | ForgeKind;
                  set({ forge, host: hostAfterForge(forge, draft.host) });
                }}
              >
                <option value="none">None</option>
                <option value="github">GitHub</option>
                <option value="gitlab">GitLab</option>
              </select>
            </Field>
            {draft.forge === 'none' ? null : (
              <>
                <Field label="Host">
                  <input
                    value={draft.host}
                    autoComplete="off"
                    placeholder={FORGE_HOST[draft.forge]}
                    onChange={(e) => set({ host: e.target.value })}
                  />
                </Field>
                <Field
                  label="Repository"
                  span={2}
                  error={shown('repo')}
                  onBlur={() => touch('repo')}
                >
                  <input
                    value={draft.repo}
                    autoComplete="off"
                    placeholder={draft.forge === 'gitlab' ? 'group/subgroup/name' : 'owner/name'}
                    onChange={(e) => set({ repo: e.target.value })}
                  />
                </Field>
              </>
            )}
          </FormGrid>
          {draft.forge === 'none' || !loaded ? null : forgeUserText !== null ? (
            <p className="fd-note" role="status">
              Signed in as {forgeUserText === '' ? 'an account' : forgeUserText}
            </p>
          ) : (
            <p className="fd-note" role="status">
              Not connected. Connect it in <a href={accountsHref}>Settings → Accounts</a>.
            </p>
          )}
        </div>
        <div className="fd-formsec" role="group" aria-label="Trackers">
          <h3 className="fd-label">Issue trackers</h3>
          {anyTracker === 0 ? (
            <p className="fd-note">
              No Jira or Linear account on this station yet.{' '}
              <a href={accountsHref}>How to connect</a>
            </p>
          ) : null}
          {draft.trackers.map((t, i) => {
            const usable = trackerAccounts(accounts, t.kind);
            const known = usable.some((r) => r.account_id === t.tracker);
            return (
              <div className="fd-trackerrow" key={i} role="group" aria-label={`Tracker ${i + 1}`}>
                <Field label="Kind">
                  <select
                    value={t.kind}
                    onChange={(e) =>
                      setTracker(i, { kind: e.target.value as TrackerKind, tracker: '' })
                    }
                  >
                    {KINDS.map((k) => (
                      <option key={k} value={k}>
                        {systemLabel(k)}
                      </option>
                    ))}
                  </select>
                </Field>
                <Field label="Account">
                  {usable.length === 0 ? (
                    <input
                      value={t.tracker}
                      autoComplete="off"
                      placeholder="Account id"
                      onChange={(e) => setTracker(i, { tracker: e.target.value })}
                    />
                  ) : (
                    <select
                      value={t.tracker}
                      onChange={(e) => setTracker(i, { tracker: e.target.value })}
                    >
                      <option value="">Choose an account</option>
                      {known || t.tracker === '' ? null : (
                        <option value={t.tracker}>{t.tracker} (not reported)</option>
                      )}
                      {usable.map((r) => (
                        <option key={r.account_id} value={r.account_id}>
                          {accountLabel(r)}
                        </option>
                      ))}
                    </select>
                  )}
                </Field>
                <ContainerPicker
                  kind={t.kind}
                  value={t.key}
                  station={draft.station}
                  accountId={known ? t.tracker : ''}
                  load={loadContainers}
                  onChange={(key) => setTracker(i, { key })}
                />
                <button
                  type="button"
                  className="fd-ghost fd-sm"
                  aria-label={`Remove tracker ${i + 1}`}
                  onClick={() => set({ trackers: draft.trackers.filter((_, at) => at !== i) })}
                >
                  <Icon of={X} size={14} /> Remove
                </button>
              </div>
            );
          })}
          {shown('trackers') === null ? null : (
            <p className="fd-fielderror" role="alert">
              {shown('trackers')}
            </p>
          )}
          <div>
            <button
              type="button"
              className="fd-ghost fd-sm"
              disabled={draft.trackers.length >= MAX_TRACKERS}
              onClick={() =>
                set({ trackers: [...draft.trackers, { tracker: '', kind: 'jira', key: '' }] })
              }
            >
              <Icon of={Plus} size={14} /> Add a tracker
            </button>
          </div>
        </div>
        <details className="fd-advanced">
          <summary>Advanced</summary>
          <FormGrid>
            <Field
              label="Prompt template (sessions from tickets)"
              hint="A prompt left empty uses the default prompt of the deck."
              span={2}
            >
              <textarea
                rows={4}
                value={draft.promptTemplate}
                maxLength={4000}
                onChange={(e) => set({ promptTemplate: e.target.value })}
              />
            </Field>
            <Field label="Review prompt (review sessions)" span={2}>
              <textarea
                rows={4}
                value={draft.reviewPrompt}
                maxLength={4000}
                onChange={(e) => set({ reviewPrompt: e.target.value })}
              />
            </Field>
          </FormGrid>
        </details>
        <FormActions>
          {error === null ? null : (
            <span className="fd-formerror" role="alert">
              {error}
            </span>
          )}
          <button type="button" className="fd-ghost" onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" className="fd-primary" disabled={saving}>
            {saving ? 'Saving…' : 'Save'}
          </button>
        </FormActions>
      </form>
      {editing && projectId !== undefined ? <ProjectGitSettings projectId={projectId} /> : null}
    </div>
  );
}
