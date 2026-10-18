// DL-172: the sections and fields of a settings view. It shows the values it is given; the tab
// keeps the edits and the state.
import type { DeckSession, MonitorInfo, SettingsView } from '../gen';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { Field, FormGrid, Section } from '../ui/page';
import { FieldEditor, GROUP_KINDS, MONITOR_KEY } from './FieldEditor';
import { InstancesEditor } from './InstancesEditor';
import { badgesOf, groupFields, shown, type Values } from './model';
import { TrackersTable } from './TrackersTable';

export type FieldsProps = {
  readonly view: SettingsView;
  readonly edits: Values;
  readonly errors: Readonly<Record<string, string>>;
  readonly onEdit: (key: string, value: unknown) => void;
  readonly stationId: string | null;
  readonly list?: ListWorkspaces | undefined;
  readonly sessions?: readonly DeckSession[] | undefined;
  /** The monitors that the station reports; `undefined` when it reports no desktop. */
  readonly monitors?: readonly MonitorInfo[] | undefined;
  /** The ids of the provider logins that the station file has now. */
  readonly savedIds: readonly string[];
  readonly secretBlocked: string | null;
  readonly onSecret: (instanceId: string, name: string | null) => void;
};

function Badges({ items }: { readonly items: ReturnType<typeof badgesOf> }) {
  return items.length === 0 ? null : (
    <span className="fd-set-badges">
      {items.map((b) => (
        <span key={b.text} className="fd-badge" data-tone={b.tone} title={b.title}>
          {b.text}
        </span>
      ))}
    </span>
  );
}

export function SettingsFields({
  view,
  edits,
  errors,
  onEdit,
  stationId,
  list,
  sessions,
  monitors,
  savedIds,
  secretBlocked,
  onSecret,
}: FieldsProps) {
  return (
    <>
      {groupFields(view.fields).map((group) => (
        <Section key={group.title} label={group.title}>
          <FormGrid>
            {group.fields.map((meta) => {
              const value = shown(view, edits, meta.key);
              const locked = meta.read_only || view.overrides.includes(meta.key);
              const wide = GROUP_KINDS.includes(meta.kind);
              const hint = (
                <>
                  {meta.help}
                  {meta.key === MONITOR_KEY && (monitors === undefined || monitors.length === 0)
                    ? ' This station reports no monitors, so type the names.'
                    : null}
                  <Badges items={badgesOf(meta, view.overrides)} />
                </>
              );
              const control =
                meta.kind === 'trackers' ? (
                  <TrackersTable value={value} />
                ) : meta.kind === 'instances' ? (
                  <InstancesEditor
                    value={value}
                    disabled={locked}
                    savedIds={savedIds}
                    secretBlocked={secretBlocked}
                    onSecret={onSecret}
                    onChange={(next) => onEdit(meta.key, next)}
                  />
                ) : (
                  <FieldEditor
                    meta={meta}
                    value={value}
                    disabled={locked}
                    stationId={stationId}
                    list={list}
                    sessions={sessions}
                    monitors={monitors}
                    onChange={(next) => onEdit(meta.key, next)}
                  />
                );
              return (
                <Field
                  key={meta.key}
                  label={meta.label}
                  hint={hint}
                  error={errors[meta.key]}
                  {...(wide ? { span: 2 as const } : {})}
                  group={wide}
                >
                  {control}
                </Field>
              );
            })}
          </FormGrid>
        </Section>
      ))}
    </>
  );
}
