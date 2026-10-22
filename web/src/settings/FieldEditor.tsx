// DL-172: the control of one settings field, by its kind. It shows the value it is given and
// reports an edit; the form keeps the edits.
import type { DeckSession, FieldMeta, MonitorInfo, StationView } from '../gen';
import { FolderPicker, type ListWorkspaces } from '../ui/FolderPicker';
import { Icon, Plus, X } from '../ui/icons';
import { defaultOptionLabel, optionLabel } from './copy';
import { asNumber, asPairs, asStrings, asText } from './model';
import { OffMinutes, PolicyEditor, StationsEditor } from './SpecialEditors';

export type EditorProps = {
  readonly meta: FieldMeta;
  readonly value: unknown;
  readonly onChange: (next: unknown) => void;
  /** The key is set by the command line: the control is shown and cannot be changed. */
  readonly disabled: boolean;
  /** The station of this tab; `null` on the deck tab. */
  readonly stationId: string | null;
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
  /** The monitors that the station reports; `undefined` when it reports no desktop. */
  readonly monitors?: readonly MonitorInfo[] | undefined;
  /** The paired stations (the stations of a worker grant are picked from them). */
  readonly stations?: readonly StationView[] | undefined;
  /** The operating system of the station of this tab (`traits.os`), for the default of `host`. */
  readonly os?: string | undefined;
};

/** The remove button of one row: an `x` icon button; its name says which row it removes. */
function RemoveButton({
  label,
  disabled,
  onClick,
}: {
  readonly label: string;
  readonly disabled: boolean;
  readonly onClick: () => void;
}) {
  return (
    <button
      type="button"
      className="fd-iconbtn fd-set-remove"
      disabled={disabled}
      aria-label={label}
      title={label}
      onClick={onClick}
    >
      <Icon of={X} size={16} />
    </button>
  );
}

/** The rows of a list of texts: one input and "Remove" for each, then an "Add" button. */
function TextRows({
  label,
  rows,
  onChange,
  disabled,
  placeholder,
  addLabel,
  mono = false,
}: {
  readonly label: string;
  readonly rows: readonly string[];
  readonly onChange: (next: string[]) => void;
  readonly disabled: boolean;
  readonly placeholder?: string | undefined;
  readonly addLabel: string;
  readonly mono?: boolean;
}) {
  return (
    <div className="fd-set-rows">
      {rows.map((row, i) => (
        <div className="fd-set-row" key={i}>
          <input
            aria-label={`${label} ${i + 1}`}
            value={row}
            disabled={disabled}
            placeholder={placeholder}
            autoComplete="off"
            spellCheck={false}
            data-mono={mono ? 'true' : undefined}
            onChange={(e) => onChange(rows.map((r, at) => (at === i ? e.target.value : r)))}
          />
          <RemoveButton
            label={`Remove ${label} ${i + 1}`}
            disabled={disabled}
            onClick={() => onChange(rows.filter((_, at) => at !== i))}
          />
        </div>
      ))}
      <div>
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={disabled}
          onClick={() => onChange([...rows, ''])}
        >
          <Icon of={Plus} size={14} /> {addLabel}
        </button>
      </div>
    </div>
  );
}

/** The key of the field whose names are picked from the monitors of the station. */
export const MONITOR_KEY = 'capture_monitors';

/** One option text of a monitor: `<name> — <description>`, or only the name. */
const monitorText = (m: MonitorInfo): string =>
  m.description === '' ? m.name : `${m.name} — ${m.description}`;

/** The rows of the monitor names: a drop-down of the station's monitors for each, then "Add". */
function MonitorRows({
  label,
  rows,
  onChange,
  disabled,
  monitors,
}: {
  readonly label: string;
  readonly rows: readonly string[];
  readonly onChange: (next: string[]) => void;
  readonly disabled: boolean;
  readonly monitors: readonly MonitorInfo[];
}) {
  const unused = monitors.find((m) => !rows.includes(m.name));
  return (
    <div className="fd-set-rows">
      {rows.map((row, i) => (
        <div className="fd-set-row" key={i}>
          <select
            aria-label={`${label} ${i + 1}`}
            value={row}
            disabled={disabled}
            onChange={(e) => onChange(rows.map((r, at) => (at === i ? e.target.value : r)))}
          >
            {monitors.some((m) => m.name === row) ? null : (
              <option value={row}>{`${row} (not connected)`}</option>
            )}
            {monitors.map((m) => (
              <option key={m.name} value={m.name}>
                {monitorText(m)}
              </option>
            ))}
          </select>
          <RemoveButton
            label={`Remove ${label} ${i + 1}`}
            disabled={disabled}
            onClick={() => onChange(rows.filter((_, at) => at !== i))}
          />
        </div>
      ))}
      <div>
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={disabled || unused === undefined}
          title={unused === undefined ? 'Every monitor is in the list.' : undefined}
          onClick={() => unused !== undefined && onChange([...rows, unused.name])}
        >
          <Icon of={Plus} size={14} /> Add
        </button>
      </div>
    </div>
  );
}

function FolderRows({
  label,
  rows,
  onChange,
  disabled,
  stationId,
  list,
  sessions,
}: {
  readonly label: string;
  readonly rows: readonly string[];
  readonly onChange: (next: string[]) => void;
  readonly disabled: boolean;
  readonly stationId: string;
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
}) {
  return (
    <div className="fd-set-rows">
      {rows.map((row, i) => (
        <div className="fd-set-row" key={i}>
          <FolderPicker
            stationId={stationId}
            value={row}
            list={list}
            sessions={sessions}
            browseDisabled={disabled}
            label={`${label} ${i + 1}`}
            onChange={(path) => onChange(rows.map((r, at) => (at === i ? path : r)))}
          />
          <RemoveButton
            label={`Remove ${label} ${i + 1}`}
            disabled={disabled}
            onClick={() => onChange(rows.filter((_, at) => at !== i))}
          />
        </div>
      ))}
      <div>
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={disabled}
          onClick={() => onChange([...rows, ''])}
        >
          <Icon of={Plus} size={14} /> Add folder
        </button>
      </div>
    </div>
  );
}

function RepoGroupRows({
  label,
  rows,
  onChange,
  disabled,
}: {
  readonly label: string;
  readonly rows: readonly (readonly [string, string])[];
  readonly onChange: (next: [string, string][]) => void;
  readonly disabled: boolean;
}) {
  const edit = (i: number, at: 0 | 1, text: string) =>
    onChange(
      rows.map((r, n): [string, string] =>
        n === i ? (at === 0 ? [text, r[1]] : [r[0], text]) : [r[0], r[1]],
      ),
    );
  return (
    <div className="fd-set-rows">
      {rows.map((row, i) => (
        <div className="fd-set-row fd-set-pair" key={i}>
          <input
            aria-label={`Repository path ${i + 1}`}
            value={row[0]}
            disabled={disabled}
            placeholder="/work/repo"
            autoComplete="off"
            spellCheck={false}
            data-mono="true"
            onChange={(e) => edit(i, 0, e.target.value)}
          />
          <input
            aria-label={`Group name ${i + 1}`}
            value={row[1]}
            disabled={disabled}
            placeholder="Group name"
            autoComplete="off"
            onChange={(e) => edit(i, 1, e.target.value)}
          />
          <RemoveButton
            label={`Remove ${label} ${i + 1}`}
            disabled={disabled}
            onClick={() => onChange(rows.filter((_, at) => at !== i).map((r) => [r[0], r[1]]))}
          />
        </div>
      ))}
      <div>
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={disabled}
          onClick={() => onChange([...rows.map((r): [string, string] => [r[0], r[1]]), ['', '']])}
        >
          <Icon of={Plus} size={14} /> Add group
        </button>
      </div>
    </div>
  );
}

/** FG-12: the Taildrive mounts as rows of "Share" and "Mount path"; the value is `{ share: path }`. */
function ShareMountRows({
  label,
  rows,
  onChange,
  disabled,
}: {
  readonly label: string;
  readonly rows: readonly (readonly [string, string])[];
  readonly onChange: (next: [string, string][]) => void;
  readonly disabled: boolean;
}) {
  const edit = (i: number, at: 0 | 1, text: string) =>
    onChange(
      rows.map((r, n): [string, string] =>
        n === i ? (at === 0 ? [text, r[1]] : [r[0], text]) : [r[0], r[1]],
      ),
    );
  return (
    <div className="fd-set-rows">
      {rows.map((row, i) => (
        <div className="fd-set-row fd-set-pair" key={i}>
          <input
            aria-label={`Share ${i + 1}`}
            value={row[0]}
            disabled={disabled}
            placeholder="nas/out"
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => edit(i, 0, e.target.value)}
          />
          <input
            aria-label={`Mount path ${i + 1}`}
            value={row[1]}
            disabled={disabled}
            placeholder="/mnt/out"
            autoComplete="off"
            spellCheck={false}
            data-mono="true"
            onChange={(e) => edit(i, 1, e.target.value)}
          />
          <RemoveButton
            label={`Remove ${label} ${i + 1}`}
            disabled={disabled}
            onClick={() => onChange(rows.filter((_, at) => at !== i).map((r) => [r[0], r[1]]))}
          />
        </div>
      ))}
      <div>
        <button
          type="button"
          className="fd-ghost fd-sm"
          disabled={disabled}
          onClick={() => onChange([...rows.map((r): [string, string] => [r[0], r[1]]), ['', '']])}
        >
          <Icon of={Plus} size={14} /> Add mount
        </button>
      </div>
    </div>
  );
}

/** The kinds whose control is made of several parts: its name is a group name, not a `<label>`. */
export const GROUP_KINDS: readonly FieldMeta['kind'][] = [
  'paths',
  'texts',
  'addresses',
  'repo_groups',
  'share_mounts',
  'instances',
  'trackers',
];

/** The keys with a control of several parts (a choice and a time, a list of checkboxes). */
export const GROUP_KEYS: readonly string[] = ['offline_approval_policy', 'worker_grant.stations'];

/** The control of the simple and list kinds. `instances` and `trackers` have their own editors. */
export function FieldEditor({
  meta,
  value,
  onChange,
  disabled,
  stationId,
  list,
  sessions,
  monitors,
  stations,
  os,
}: EditorProps) {
  if (meta.key === 'offline_approval_policy' && meta.kind === 'text') {
    return <PolicyEditor value={asText(value)} disabled={disabled} onChange={onChange} />;
  }
  if (meta.key === 'worker_grant.stations' && meta.kind === 'texts' && stations !== undefined) {
    return (
      <StationsEditor
        value={asStrings(value)}
        stations={stations}
        disabled={disabled}
        onChange={onChange}
      />
    );
  }
  if (meta.key === 'hibernate_after_min' && meta.kind === 'int') {
    return (
      <OffMinutes
        label={meta.label}
        value={asNumber(value)}
        min={meta.min}
        max={meta.max}
        disabled={disabled}
        onChange={onChange}
      />
    );
  }
  switch (meta.kind) {
    case 'bool':
      return (
        <input
          type="checkbox"
          role="switch"
          className="fd-switch"
          aria-checked={value === true}
          checked={value === true}
          disabled={disabled}
          onChange={(e) => onChange(e.target.checked)}
        />
      );
    case 'int': {
      const n = asNumber(value);
      return (
        <input
          type="number"
          inputMode="numeric"
          value={n ?? ''}
          min={meta.min ?? undefined}
          max={meta.max ?? undefined}
          disabled={disabled}
          onChange={(e) => onChange(e.target.value === '' ? null : Number(e.target.value))}
        />
      );
    }
    case 'enum': {
      const text = asText(value);
      return (
        <select value={text} disabled={disabled} onChange={(e) => onChange(e.target.value)}>
          <option value="">{defaultOptionLabel(meta.key, os)}</option>
          {meta.options.map((o) => (
            <option key={o} value={o}>
              {optionLabel(meta.key, o)}
            </option>
          ))}
        </select>
      );
    }
    case 'path':
      return stationId === null ? (
        <input
          value={asText(value)}
          disabled={disabled}
          autoComplete="off"
          spellCheck={false}
          data-mono="true"
          onChange={(e) => onChange(e.target.value)}
        />
      ) : (
        <FolderPicker
          stationId={stationId}
          value={asText(value)}
          list={list}
          sessions={sessions}
          browseDisabled={disabled}
          label={meta.label}
          onChange={onChange}
        />
      );
    case 'paths':
      return stationId === null ? (
        <TextRows
          label={meta.label}
          rows={asStrings(value)}
          onChange={onChange}
          disabled={disabled}
          addLabel="Add folder"
          mono
        />
      ) : (
        <FolderRows
          label={meta.label}
          rows={asStrings(value)}
          onChange={onChange}
          disabled={disabled}
          stationId={stationId}
          list={list}
          sessions={sessions}
        />
      );
    case 'texts':
      if (meta.key === MONITOR_KEY && monitors !== undefined && monitors.length > 0) {
        return (
          <MonitorRows
            label={meta.label}
            rows={asStrings(value)}
            onChange={onChange}
            disabled={disabled}
            monitors={monitors}
          />
        );
      }
      return (
        <TextRows
          label={meta.label}
          rows={asStrings(value)}
          onChange={onChange}
          disabled={disabled}
          addLabel="Add"
        />
      );
    case 'addresses':
      return (
        <TextRows
          label={meta.label}
          rows={asStrings(value)}
          onChange={onChange}
          disabled={disabled}
          placeholder="127.0.0.1:7421"
          addLabel="Add address"
          mono
        />
      );
    case 'repo_groups':
      return (
        <RepoGroupRows
          label={meta.label}
          rows={asPairs(value)}
          onChange={onChange}
          disabled={disabled}
        />
      );
    case 'share_mounts':
      return (
        <ShareMountRows
          label={meta.label}
          rows={asPairs(value)}
          onChange={onChange}
          disabled={disabled}
        />
      );
    case 'text':
      return (
        <input
          value={asText(value)}
          disabled={disabled}
          autoComplete="off"
          onChange={(e) => onChange(e.target.value)}
        />
      );
    default:
      return null;
  }
}
