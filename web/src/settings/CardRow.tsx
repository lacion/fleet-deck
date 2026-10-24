// One row of a station card (Omarchy, Desktop tools) on the field grid of the settings page
// (§4.7, `.fd-fieldwrap[data-layout="row"]`): a label in the 200px column, then the facts, the
// notices and the buttons of the card in the control column, so that the card keeps the columns
// of the sections around it. Under 720px the row stacks, as every field row does.
import { type ReactNode, useId } from 'react';
import type { CardFact } from './omarchy';

export function CardRow({
  label,
  className,
  children,
}: {
  readonly label: string;
  readonly className?: string;
  readonly children: ReactNode;
}) {
  const id = useId();
  return (
    <div
      className={className === undefined ? 'fd-fieldwrap' : `fd-fieldwrap ${className}`}
      data-layout="row"
      role="group"
      aria-labelledby={id}
    >
      <span id={id} className="fd-fieldlabel">
        {label}
      </span>
      <div className="fd-set-cardcol">{children}</div>
    </div>
  );
}

/** The facts of a card as a grid of rows: a status glyph, the name and the state (§3 `.fd-dot`). */
export function FactList({ facts }: { readonly facts: readonly CardFact[] }) {
  return (
    <ul className="fd-dt-tools">
      {facts.map((f) => (
        <li key={f.name}>
          <span className="fd-dot" data-status={f.status} aria-hidden="true" />
          <span>{f.name}</span>
          <span className="fd-note">{f.state}</span>
        </li>
      ))}
    </ul>
  );
}
