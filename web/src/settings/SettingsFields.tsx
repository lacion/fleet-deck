// DL-172: the sections and fields of a settings view. It shows the values it is given; the tab
// keeps the edits and the state. DESIGN-SYSTEM §4.7: each group is a section with a heading and no
// box; its fields sit in a two-column grid (the label, then the control and its hint). The badges
// of a field ("Applies after a restart") sit in one place: after a one-value control, on its line;
// under the label of a list. A field whose save asks for the password has a lock after its label;
// when every field of a section has one, the section says it once next to its heading.
import { Fragment, type ReactNode, useId } from 'react';
import type { DeckSession, FieldMeta, MonitorInfo, SettingsView, StationView } from '../gen';
import type { ListWorkspaces } from '../ui/FolderPicker';
import { Icon, Lock, TriangleAlert } from '../ui/icons';
import { DEPENDS_ON, fieldLabel, helpOf } from './copy';
import { FieldEditor, GROUP_KEYS, GROUP_KINDS, MONITOR_KEY } from './FieldEditor';
import { InstancesEditor } from './InstancesEditor';
import { allDanger, badgesOf, groupFields, sectionId, shown, type Values } from './model';
import { TrackersTable } from './TrackersTable';

/** What a tab adds to a section: a card under its heading, or sections after it. */
export type SectionSlot = { readonly top?: ReactNode; readonly after?: ReactNode };

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
  /** The paired stations (the worker grant stations). */
  readonly stations?: readonly StationView[] | undefined;
  /** The operating system of the station of this tab (`traits.os`). */
  readonly os?: string | undefined;
  /**
   * The cards of the tab by section title. A section with a `top` card shows also without a
   * field; the `after` part of a section that is not on the page goes to the end.
   */
  readonly slots?: Readonly<Record<string, SectionSlot>> | undefined;
  /** The keys that a card of the tab shows now: their fields are left out (model.ts). */
  readonly omit?: readonly string[] | undefined;
};

/** The words of the password mark (a section note, the title and the hidden text of a field). */
export const DANGER_TEXT = 'Saving asks for your password';

type BadgeItems = ReturnType<typeof badgesOf>;

/**
 * The badges of a field. They sit inside the label of the field, so they are hidden from assistive
 * technology (the control keeps its name); the hint carries their words.
 */
function Badges({ items }: { readonly items: BadgeItems }) {
  return items.length === 0 ? null : (
    <span className="fd-set-badges" aria-hidden="true">
      {items.map((b) => (
        <span key={b.text} className="fd-badge">
          {b.text}
        </span>
      ))}
    </span>
  );
}

/**
 * One field row of the settings grid: the markup of `Field` (ui/page.tsx), with a mark after the
 * label text, the badges and a line after the hint. The mark and the badges are hidden from
 * assistive technology, so that the control keeps its name; the hint carries their words. The
 * badges of a one-value control end its control line; the badges of a list (a group) sit under
 * its label, where they do not move with the length of the list.
 */
function FieldRow({
  label,
  mark,
  badges,
  hint,
  error,
  extra,
  group,
  sub,
  children,
}: {
  readonly label: string;
  readonly mark: ReactNode;
  readonly badges: BadgeItems;
  readonly hint: ReactNode;
  readonly error: string | undefined;
  readonly extra: ReactNode;
  readonly group: boolean;
  /** The switch works only while the one above it is on: its label is indented under it. */
  readonly sub: boolean;
  readonly children: ReactNode;
}) {
  const id = useId();
  const failed = error !== undefined && error !== '';
  // One flex item, so that the mark follows the last word of a label that wraps.
  const text =
    mark === null ? (
      label
    ) : (
      <span>
        {label}
        {mark}
      </span>
    );
  const name =
    group && badges.length > 0 ? (
      <span id={id} className="fd-fieldlabel fd-set-labelbadges">
        <span className="fd-set-labeltext">{text}</span>
        <Badges items={badges} />
      </span>
    ) : (
      <span id={group ? id : undefined} className="fd-fieldlabel">
        {text}
      </span>
    );
  return (
    <div
      className="fd-fieldwrap"
      data-layout="row"
      data-span={group ? 2 : undefined}
      data-invalid={failed ? 'true' : undefined}
      data-sub={sub ? 'true' : undefined}
      role={group ? 'group' : undefined}
      aria-labelledby={group ? id : undefined}
    >
      {group ? (
        <>
          {name}
          {children}
        </>
      ) : (
        // biome-ignore lint/a11y/noLabelWithoutControl: the control is the child of the field
        <label className="fd-fieldcontrol">
          {name}
          {badges.length === 0 ? (
            children
          ) : (
            <span className="fd-set-ctlrow">
              {children}
              <Badges items={badges} />
            </span>
          )}
        </label>
      )}
      {failed ? (
        <span className="fd-fielderror" role="alert">
          {error}
        </span>
      ) : (
        <span className="fd-fieldhint">{hint}</span>
      )}
      {extra}
    </div>
  );
}

/** The lock after the label of a field whose save asks for the password. */
const LOCK = (
  <span className="fd-set-lock" title={DANGER_TEXT} aria-hidden="true">
    <Icon of={Lock} size={12} />
  </span>
);

/** The warning under the bypass switch while it is on (§2.2: bypass is a hazard state). */
function hazardOf(meta: FieldMeta, value: unknown): ReactNode {
  if (meta.key !== 'allow_bypass_permissions' || value !== true) return null;
  return (
    <p className="fd-set-hazard">
      <Icon of={TriangleAlert} size={14} />
      Sessions on this station can run every tool without asking.
    </p>
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
  stations,
  os,
  slots = {},
  omit = [],
}: FieldsProps) {
  const keep = Object.entries(slots)
    .filter(([, slot]) => slot.top !== undefined)
    .map(([title]) => title);
  const groups = groupFields(view.fields, keep, omit);
  const titles = groups.map((g) => g.title);
  const orphans = Object.entries(slots).filter(
    ([title, slot]) => slot.after !== undefined && !titles.includes(title),
  );
  return (
    <>
      {groups.map((group) => {
        const once = allDanger(group);
        const slot = slots[group.title];
        return (
          <Fragment key={group.title}>
            <section id={sectionId(group.title)} className="fd-set-sec" aria-label={group.title}>
              <div className="fd-set-sechead-row">
                <h2 className="fd-set-sechead">{group.title}</h2>
                {once ? (
                  <span className="fd-set-secnote">
                    <Icon of={Lock} size={12} /> {DANGER_TEXT}
                  </span>
                ) : null}
              </div>
              {slot?.top ?? null}
              {group.fields.length === 0 ? null : (
                <div className="fd-set-grid">
                  {group.fields.map((meta) => {
                    const value = shown(view, edits, meta.key);
                    const locked = meta.read_only || view.overrides.includes(meta.key);
                    const parts = GROUP_KINDS.includes(meta.kind) || GROUP_KEYS.includes(meta.key);
                    const marked = meta.danger && !once;
                    const badges = badgesOf(meta, view.overrides);
                    const hint = (
                      <>
                        {helpOf(meta, stationId === null)}
                        {meta.key === MONITOR_KEY &&
                        (monitors === undefined || monitors.length === 0)
                          ? ' This station reports no monitors, so type the names.'
                          : null}
                        {marked ? <span className="fd-vh">{` ${DANGER_TEXT}.`}</span> : null}
                        {badges.length === 0 ? null : (
                          <span className="fd-vh">{badges.map((b) => ` ${b.text}.`).join('')}</span>
                        )}
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
                          stations={stations}
                          os={os}
                          onChange={(next) => onEdit(meta.key, next)}
                        />
                      );
                    return (
                      <FieldRow
                        key={meta.key}
                        label={fieldLabel(meta)}
                        mark={marked ? LOCK : null}
                        badges={badges}
                        hint={hint}
                        error={errors[meta.key]}
                        extra={hazardOf(meta, value)}
                        group={parts}
                        sub={DEPENDS_ON[meta.key] !== undefined}
                      >
                        {control}
                      </FieldRow>
                    );
                  })}
                </div>
              )}
            </section>
            {slot?.after ?? null}
          </Fragment>
        );
      })}
      {orphans.map(([title, slot]) => (
        <Fragment key={`after:${title}`}>{slot.after}</Fragment>
      ))}
    </>
  );
}
