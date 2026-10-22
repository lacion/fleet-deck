// R13 P3c (design §6.3, §6D item 6): the forms of the Desktop panel: workspace, notification, link
// and wall open. Each form sends one `DesktopActBody` through `run`; the deck checks every value.
import { type FormEvent, useEffect, useState } from 'react';
import type { DesktopActBody, DesktopInfo, DisplayView } from '../gen';
import { describe } from '../settings/http';
import type { DisplayActions } from '../displays/actions';
import { Field } from '../ui/page';

export type Run = (body: DesktopActBody, done: string) => void;

const MAX_TITLE = 80;
const MAX_BODY = 300;
const MAX_URL = 2048;

function MonitorSelect({
  info,
  value,
  onChange,
  optional,
  label,
}: {
  readonly info: DesktopInfo;
  readonly value: string;
  readonly onChange: (v: string) => void;
  readonly optional: boolean;
  readonly label: string;
}) {
  return (
    <Field label={label}>
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        {optional ? <option value="">Any monitor</option> : null}
        {info.monitors.map((m) => (
          <option key={m.name} value={m.name}>
            {m.name}
          </option>
        ))}
      </select>
    </Field>
  );
}

const firstMonitor = (info: DesktopInfo): string => info.monitors[0]?.name ?? '';

export function WorkspaceForm({
  info,
  run,
  busy,
}: {
  readonly info: DesktopInfo;
  readonly run: Run;
  readonly busy: boolean;
}) {
  const [monitor, setMonitor] = useState(firstMonitor(info));
  const [workspace, setWorkspace] = useState('1');
  const [error, setError] = useState<string | null>(null);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const n = Number(workspace);
    if (monitor === '' || !Number.isInteger(n) || n < 1 || n > 99) {
      setError('Choose a monitor and a workspace from 1 to 99.');
      return;
    }
    setError(null);
    run({ action: 'workspace_show', monitor, workspace: n }, `Showing workspace ${n}.`);
  };
  return (
    <form className="fd-desk-form" onSubmit={submit} aria-label="Show a workspace">
      <MonitorSelect
        info={info}
        value={monitor}
        onChange={setMonitor}
        optional={false}
        label="Monitor"
      />
      <Field label="Workspace">
        <input
          type="number"
          min={1}
          max={99}
          value={workspace}
          onChange={(e) => setWorkspace(e.target.value)}
        />
      </Field>
      {error === null ? null : (
        <p className="fd-fielderror" role="alert">
          {error}
        </p>
      )}
      <button type="submit" className="fd-sm" disabled={busy}>
        Show workspace
      </button>
    </form>
  );
}

export function NotifyForm({ run, busy }: { readonly run: Run; readonly busy: boolean }) {
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [urgency, setUrgency] = useState<'low' | 'normal' | 'critical'>('normal');
  const [error, setError] = useState<string | null>(null);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (title.trim() === '') {
      setError('Give a title.');
      return;
    }
    setError(null);
    run({ action: 'notify', title: title.trim(), body, urgency }, 'Notification sent.');
  };
  return (
    <form className="fd-desk-form" onSubmit={submit} aria-label="Send a notification">
      <Field label="Title">
        <input value={title} maxLength={MAX_TITLE} onChange={(e) => setTitle(e.target.value)} />
      </Field>
      <Field label="Message">
        <textarea value={body} maxLength={MAX_BODY} onChange={(e) => setBody(e.target.value)} />
      </Field>
      <Field label="Urgency">
        <select
          value={urgency}
          onChange={(e) => setUrgency(e.target.value as 'low' | 'normal' | 'critical')}
        >
          <option value="low">Low</option>
          <option value="normal">Normal</option>
          <option value="critical">Critical</option>
        </select>
      </Field>
      {error === null ? null : (
        <p className="fd-fielderror" role="alert">
          {error}
        </p>
      )}
      <button type="submit" className="fd-sm" disabled={busy}>
        Send notification
      </button>
    </form>
  );
}

/** True for an `http` or `https` URL of at most 2048 bytes. */
export function isWebUrl(text: string): boolean {
  if (new TextEncoder().encode(text).length > MAX_URL) return false;
  try {
    const u = new URL(text);
    return u.protocol === 'http:' || u.protocol === 'https:';
  } catch {
    return false;
  }
}

export function LinkForm({
  info,
  run,
  busy,
}: {
  readonly info: DesktopInfo;
  readonly run: Run;
  readonly busy: boolean;
}) {
  const [url, setUrl] = useState('');
  const [monitor, setMonitor] = useState('');
  const [error, setError] = useState<string | null>(null);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!isWebUrl(url.trim())) {
      setError('Give an http or https link.');
      return;
    }
    setError(null);
    run(
      {
        action: 'browser_open',
        url: url.trim(),
        monitor: monitor === '' ? null : monitor,
        session: null,
      },
      'Opened the link.',
    );
  };
  return (
    <form className="fd-desk-form" onSubmit={submit} aria-label="Open a link">
      <Field label="Link">
        <input type="url" value={url} onChange={(e) => setUrl(e.target.value)} />
      </Field>
      <MonitorSelect info={info} value={monitor} onChange={setMonitor} optional label="Monitor" />
      {error === null ? null : (
        <p className="fd-fielderror" role="alert">
          {error}
        </p>
      )}
      <button type="submit" className="fd-sm" disabled={busy}>
        Open the link
      </button>
    </form>
  );
}

export function WallOpenForm({
  info,
  run,
  busy,
  displays,
}: {
  readonly info: DesktopInfo;
  readonly run: Run;
  readonly busy: boolean;
  readonly displays: DisplayActions;
}) {
  const [list, setList] = useState<readonly DisplayView[] | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [display, setDisplay] = useState('');
  const [monitor, setMonitor] = useState(firstMonitor(info));
  const [workspace, setWorkspace] = useState('');
  useEffect(() => {
    let live = true;
    displays.list().then(
      (l) => {
        if (!live) return;
        setList(l.displays);
        setDisplay(l.displays[0]?.display_id ?? '');
      },
      (e: unknown) => live && setProblem(describe(e)),
    );
    return () => {
      live = false;
    };
  }, [displays]);
  const submit = (event: FormEvent) => {
    event.preventDefault();
    const n = workspace === '' ? null : Number(workspace);
    if (
      display === '' ||
      monitor === '' ||
      (n !== null && (!Number.isInteger(n) || n < 1 || n > 99))
    ) {
      setProblem('Choose a display and a monitor, and a workspace from 1 to 99 or none.');
      return;
    }
    setProblem(null);
    run({ action: 'wall_open', display_id: display, monitor, workspace: n }, 'Opening the wall.');
  };
  return (
    <form className="fd-desk-form" onSubmit={submit} aria-label="Open a wall">
      <Field label="Display">
        <select value={display} onChange={(e) => setDisplay(e.target.value)}>
          {(list ?? []).map((d) => (
            <option key={d.display_id} value={d.display_id}>
              {d.name}
            </option>
          ))}
        </select>
      </Field>
      <MonitorSelect
        info={info}
        value={monitor}
        onChange={setMonitor}
        optional={false}
        label="Monitor"
      />
      <Field label="Workspace (optional)">
        <input
          type="number"
          min={1}
          max={99}
          value={workspace}
          onChange={(e) => setWorkspace(e.target.value)}
        />
      </Field>
      {problem === null ? null : (
        <p className="fd-fielderror" role="alert">
          {problem}
        </p>
      )}
      <button type="submit" className="fd-sm" disabled={busy || list === null || list.length === 0}>
        Open the wall
      </button>
    </form>
  );
}
