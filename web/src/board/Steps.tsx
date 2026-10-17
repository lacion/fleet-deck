type Props = {
  readonly labels: readonly string[];
  /** The index of the current step. */
  readonly current: number;
  /** Every step is finished. */
  readonly done?: boolean;
};

/** The numbered steps of a guided flow. Purely presentational. */
export function Steps({ labels, current, done = false }: Props) {
  return (
    <ol className="fd-steps">
      {labels.map((label, index) => {
        const finished = done || index < current;
        return (
          <li
            key={label}
            className="fd-steps-item"
            data-state={finished ? 'done' : index === current ? 'current' : 'next'}
            aria-current={!done && index === current ? 'step' : undefined}
          >
            <span className="fd-steps-mark" aria-hidden="true">
              {finished ? '✓' : index + 1}
            </span>
            <span className="fd-steps-label">{label}</span>
          </li>
        );
      })}
    </ol>
  );
}
