// DL-140: the settings of a session as menu rows: the mode ladder (the Properties Mode row, or the
// session menu where no Properties panel shows), the model picker (the composer model chip, or the
// phone session menu) and the auto-continue switch of the session menu.
import { useId, useState } from 'react';
import type { ModelChoice } from '../gen';
import { contextLabel, isRiskyMode, modeLabel, modelFamily, shortModel } from '../ui/format';
import { Menu } from '../ui/Menu';
import { Check, ChevronDown, Icon, RotateCcw, ShieldAlert } from '../ui/icons';

export function ModeLadder(props: {
  modes: readonly string[];
  current: string | null;
  enabled: boolean;
  onSet: (mode: string) => void;
}) {
  // The capability lists the modes from the most restrictive (L8): the order of the rows says it.
  return (
    <fieldset className="fd-modes">
      <legend>Mode</legend>
      {/* One menu row per mode, with its sentence-case name (the title keeps the raw id): a mode
          that skips the approvals has the shield-alert icon, the current mode is pressed and has
          a check. A wrapped segmented control did not read as one control in the menu. */}
      <div className="fd-modes-list">
        {props.modes.map((m) => {
          const current = m === props.current;
          return (
            <button
              key={m}
              type="button"
              className="fd-mode-row fd-selbar"
              title={m}
              aria-pressed={current}
              disabled={!props.enabled || current}
              onClick={() => props.onSet(m)}
            >
              <span className="fd-mode-lead" data-risky={isRiskyMode(m) || undefined}>
                {isRiskyMode(m) ? <Icon of={ShieldAlert} size={14} /> : null}
              </span>
              {modeLabel(m)}
              {current ? <Icon of={Check} size={14} /> : null}
            </button>
          );
        })}
      </div>
    </fieldset>
  );
}

/**
 * The name of a model row: the provider's label (`Default (recommended)`, `Fable 5.6`); a label
 * that is only the raw value (the provider gave no name) shows the short model name instead
 * (`claude-sonnet-5-5` → `Sonnet 5.5`). A shortened id would merge two ids of one version.
 */
export function modelRowName(value: string, label: string): string {
  if (label !== '' && label !== value) return label;
  return modelFamily(value) === null && !/^gpt-/i.test(value) ? value : shortModel(value);
}

/** One row of the model picker: its name, and a --dim aside (the context window, or the id). */
export type ModelRowText = {
  readonly value: string;
  readonly name: string;
  /** The context window (`1M context`) when the name does not say it already. */
  readonly context: string | null;
  /** The raw id, when two rows have the same name (else they read as one model). */
  readonly id: string | null;
  readonly title: string;
};

/** The rows of a model list: names, asides and titles (the raw id stays in the title). */
export function modelRows(list: readonly ModelChoice[]): ModelRowText[] {
  const names = list.map((m) => modelRowName(m.value, m.label));
  return list.map((m, i) => {
    const name = names[i] ?? m.value;
    const suffix = /\[([^\]]+)\]$/.exec(m.value)?.[1];
    const said = suffix !== undefined && name.toLowerCase().includes(suffix.toLowerCase());
    const twice = names.filter((n) => n === name).length > 1;
    return {
      value: m.value,
      name,
      context: said ? null : contextLabel(m.value),
      id: twice ? m.value : null,
      title: m.description === null ? m.value : `${m.value}: ${m.description}`,
    };
  });
}

/** One model row: the name, its asides in --dim, and a check on the current model. */
function ModelRow(props: {
  row: Omit<ModelRowText, 'value'>;
  current: boolean;
  enabled: boolean;
  onPick: () => void;
}) {
  const { row } = props;
  return (
    <button
      type="button"
      className="fd-mode-row fd-model-row fd-selbar"
      title={row.title}
      aria-pressed={props.current}
      disabled={!props.enabled || props.current}
      onClick={props.onPick}
    >
      <span className="fd-model-name">{row.name}</span>
      {row.context === null ? null : <span className="fd-model-context">{row.context}</span>}
      {row.id === null ? null : <code className="fd-model-id">{row.id}</code>}
      {props.current ? <Icon of={Check} size={14} /> : null}
    </button>
  );
}

/**
 * The models as menu rows (DL-151): the models of the provider, a check on the current one, and
 * "Other…" for an id or alias that the board cannot list (the only control without a list). A
 * current model that is in no row of the list gets a row of its own first, so the picker always
 * shows the model that runs (its raw id stays in the title). `onDone` closes the menu after a pick.
 */
export function ModelPicker(props: {
  current: string | null;
  models: readonly ModelChoice[] | null;
  enabled: boolean;
  onSet: (model: string) => void;
  onDone?: (() => void) | undefined;
}) {
  const [model, setModel] = useState('');
  const [other, setOther] = useState(false);
  const id = useId();
  const value = model.trim();
  const list = props.models !== null && props.models.length > 0 ? props.models : null;
  const pick = (next: string) => {
    props.onDone?.();
    props.onSet(next);
  };
  const apply = () => {
    if (value === '') return;
    setModel('');
    pick(value);
  };
  const current = props.current;
  const listed = list?.some((m) => m.value === current) === true;
  return (
    <fieldset className="fd-modes fd-models">
      <legend>Model</legend>
      <div className="fd-modes-list">
        {current === null || current === '' || listed ? null : (
          <ModelRow
            row={{
              name: shortModel(current),
              context: contextLabel(current),
              id: null,
              title: current,
            }}
            current
            enabled={props.enabled}
            onPick={() => {}}
          />
        )}
        {modelRows(list ?? []).map((row) => (
          <ModelRow
            key={row.value}
            row={row}
            current={row.value === current}
            enabled={props.enabled}
            onPick={() => pick(row.value)}
          />
        ))}
        {list === null ? null : (
          <button
            type="button"
            className="fd-mode-row"
            aria-expanded={other}
            aria-controls={other ? id : undefined}
            disabled={!props.enabled}
            onClick={() => setOther(!other)}
          >
            Other…
          </button>
        )}
      </div>
      {/* Not a form: the composer chip puts the picker inside the composer form, and a submit
          here would also send the draft. Enter in the field sets the model, and only that. */}
      {list === null || other ? (
        <div className="fd-model-other" id={id} role="group" aria-label="Model id">
          <input
            className="fd-sm"
            value={model}
            disabled={!props.enabled}
            onChange={(event) => setModel(event.target.value)}
            onKeyDown={(event) => {
              if (event.key !== 'Enter' || event.nativeEvent.isComposing) return;
              event.preventDefault();
              event.stopPropagation();
              apply();
            }}
            placeholder="Model id or alias"
            aria-label="Model id or alias"
          />
          <button
            type="button"
            className="fd-sm"
            disabled={!props.enabled || value === ''}
            onClick={apply}
          >
            Set model
          </button>
        </div>
      ) : null}
    </fieldset>
  );
}

/**
 * The composer model chip: the summary of its own popover, which opens upward from the chip and
 * lists the models (§4.5). `name` is the short name of the current model.
 */
export function ModelMenu(props: {
  name: string;
  current: string | null;
  models: readonly ModelChoice[] | null;
  onSet: (model: string) => void;
}) {
  return (
    <Menu
      summary={
        <>
          {props.name}
          <Icon of={ChevronDown} size={12} />
        </>
      }
      summaryClass="fd-sm fd-composer-model"
      label={`Model: ${props.name}`}
      title="Change the model"
    >
      {(close) => (
        <ModelPicker
          current={props.current}
          models={props.models}
          enabled
          onSet={props.onSet}
          onDone={close}
        />
      )}
    </Menu>
  );
}

/** "Continue after a station restart": a named switch row of the session menu (§4.2). */
export function AutoContinueSwitch(props: {
  checked: boolean;
  enabled: boolean;
  onSet: (on: boolean) => void;
}) {
  return (
    <label className="fd-menu-switch" data-disabled={props.enabled ? undefined : 'true'}>
      <span className="fd-menu-switch-label">
        <Icon of={RotateCcw} size={14} />
        Continue after a station restart
      </span>
      <input
        type="checkbox"
        role="switch"
        className="fd-switch"
        aria-checked={props.checked}
        checked={props.checked}
        disabled={!props.enabled}
        onChange={(event) => props.onSet(event.currentTarget.checked)}
      />
    </label>
  );
}
