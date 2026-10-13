// DL-140: the mode ladder and the model picker, moved out of SessionView for the
// settings menu of the session header (SessionView keeps its own copies until wave 2).
import { type FormEvent, useId, useState } from 'react';
import type { ModelChoice } from '../gen';

export function ModeLadder(props: {
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
          className="fd-sm"
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

const OTHER = '__other__';

export function ModelPicker(props: {
  current: string | null;
  models: readonly ModelChoice[] | null;
  enabled: boolean;
  onSet: (model: string) => void;
}) {
  // Providers accept model ids and aliases that the board cannot list, so the
  // picker keeps a free-text form: the only control without a model list, and
  // behind "Other…" with one (DL-060, DL-151).
  const [model, setModel] = useState('');
  const [other, setOther] = useState(false);
  const id = useId();
  const value = model.trim();
  const list = props.models !== null && props.models.length > 0 ? props.models : null;
  const label = <label htmlFor={id}>Model (now {props.current ?? 'unknown'})</label>;
  const text = (
    <>
      <input
        id={list ? undefined : id}
        className="fd-sm"
        value={model}
        disabled={!props.enabled}
        onChange={(event) => setModel(event.target.value)}
        placeholder="Model id or alias"
        {...(list ? { 'aria-label': 'Model id or alias' } : {})}
      />
      <button type="submit" className="fd-sm" disabled={!props.enabled || value === ''}>
        Set model
      </button>
    </>
  );
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (value === '') return;
    setModel('');
    props.onSet(value);
  };
  if (list === null) {
    return (
      <form className="fd-model" aria-label="Model" onSubmit={submit}>
        {label}
        {text}
      </form>
    );
  }
  const matches = list.some((m) => m.value === props.current);
  const selected = other ? OTHER : matches && props.current !== null ? props.current : '';
  return (
    <div className="fd-model">
      {label}
      <select
        id={id}
        className="fd-sm fd-model-select"
        aria-label="Model"
        value={selected}
        disabled={!props.enabled}
        onChange={(event) => {
          const next = event.target.value;
          if (next === OTHER) {
            setOther(true);
            return;
          }
          setOther(false);
          props.onSet(next);
        }}
      >
        <option value="" disabled>
          Choose a model
        </option>
        {list.map((m) => (
          <option key={m.value} value={m.value} title={m.description ?? undefined}>
            {m.label}
          </option>
        ))}
        <option value={OTHER}>Other…</option>
      </select>
      {other ? (
        <form className="fd-model-other" onSubmit={submit}>
          {text}
        </form>
      ) : null}
    </div>
  );
}
