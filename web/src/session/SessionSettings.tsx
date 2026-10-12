// DL-140: the mode ladder and the model picker, moved out of SessionView for the
// settings menu of the session header (SessionView keeps its own copies until wave 2).
import { useId, useState } from 'react';

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

export function ModelPicker(props: {
  current: string | null;
  enabled: boolean;
  onSet: (model: string) => void;
}) {
  // Providers accept model ids and aliases that the board cannot list, so the
  // picker takes free text (DL-060).
  const [model, setModel] = useState('');
  const id = useId();
  const value = model.trim();
  return (
    <form
      className="fd-model"
      aria-label="Model"
      onSubmit={(event) => {
        event.preventDefault();
        if (value === '') return;
        setModel('');
        props.onSet(value);
      }}
    >
      <label htmlFor={id}>Model (now {props.current ?? 'unknown'})</label>
      <input
        id={id}
        className="fd-sm"
        value={model}
        disabled={!props.enabled}
        onChange={(event) => setModel(event.target.value)}
        placeholder="Model id or alias"
      />
      <button type="submit" className="fd-sm" disabled={!props.enabled || value === ''}>
        Set model
      </button>
    </form>
  );
}
