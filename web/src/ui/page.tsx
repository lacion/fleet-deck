// DL-171: the frame of every secondary page of the board and its form parts (UI-DESIGN.md §3.6).
import { type ReactNode, useId } from 'react';
import { ChevronLeft, Icon, type LucideIcon, OctagonAlert } from './icons';

type PageProps = {
  readonly title: string;
  /** A count next to the title (`.fd-count`). */
  readonly count?: number | string | undefined;
  /** One or two sentences under the title. */
  readonly description?: ReactNode;
  /**
   * The filters of the page (§4.3: `.fd-seg`, chips, selects, a search) in the header row at the
   * right of the title, before the actions. A narrow column wraps them under the title.
   */
  readonly filters?: ReactNode;
  /** The page actions, at the right of the title. */
  readonly actions?: ReactNode;
  /**
   * The body fills the main column, as the fleet home (tables, lists, grids, dashboards). Without
   * it the body keeps a 1040 px measure (forms, prose). The header runs the full column on every
   * page, so that its rule has one length (§4.3).
   */
  readonly wide?: boolean;
  /** A link back to the parent list ('‹ All projects'), above the title (§4.3). */
  readonly back?: { readonly href: string; readonly label: string } | undefined;
  /** The accessible name of the section; the title by default. */
  readonly label?: string | undefined;
  /** An extra class of the section. */
  readonly className?: string | undefined;
  readonly children: ReactNode;
};

/**
 * The page frame: padding, a left-aligned column, and the page header (DESIGN-SYSTEM §4.3): the
 * title and its count with the description under them at the left, the filters and then the
 * actions at the right, then a rule. A back link is a row of its own above them all, so that the
 * filters and the actions stay level with the title row.
 */
export function Page({
  title,
  count,
  description,
  filters,
  actions,
  wide = false,
  back,
  label,
  className,
  children,
}: PageProps) {
  return (
    <section
      className={className === undefined ? 'fd-page' : `fd-page ${className}`}
      data-wide={wide ? 'true' : undefined}
      aria-label={label ?? title}
    >
      <header className="fd-page-head">
        {back === undefined ? null : (
          <div className="fd-page-backrow">
            <a className="fd-btn fd-ghost fd-sm fd-page-back" href={back.href}>
              <Icon of={ChevronLeft} size={14} /> {back.label}
            </a>
          </div>
        )}
        <div className="fd-page-titles">
          <div className="fd-page-titlerow">
            <h1 className="fd-page-title">{title}</h1>
            {count === undefined ? null : <span className="fd-count">{count}</span>}
          </div>
          {description === undefined || description === null ? null : (
            <p className="fd-page-desc">{description}</p>
          )}
        </div>
        {filters === undefined || filters === null ? null : (
          <div className="fd-page-filters">{filters}</div>
        )}
        {actions === undefined || actions === null ? null : (
          <div className="fd-page-actions">{actions}</div>
        )}
      </header>
      {children}
    </section>
  );
}

/** A labelled block of a page: a 15px heading row, then the content in a `.fd-panel` (§4.7). */
export function Section({
  label,
  actions,
  children,
}: {
  readonly label: string;
  readonly actions?: ReactNode;
  readonly children: ReactNode;
}) {
  return (
    <section className="fd-pagesec" aria-label={label}>
      <div className="fd-pagesec-head">
        <h2 className="fd-pagesec-title">{label}</h2>
        {actions === undefined ? null : <div className="fd-page-actions">{actions}</div>}
      </div>
      <div className="fd-panel fd-pagesec-body">{children}</div>
    </section>
  );
}

/** What an empty list shows, with no box: a 20px icon, a title, a sentence and one action (§4.7). */
export function EmptyState({
  icon,
  title,
  body,
  action,
}: {
  readonly icon: LucideIcon;
  readonly title: string;
  readonly body: ReactNode;
  readonly action?: ReactNode;
}) {
  return (
    <div className="fd-emptystate">
      <Icon of={icon} size={20} />
      <p className="fd-emptystate-title">{title}</p>
      <p className="fd-emptystate-body">{body}</p>
      {action === undefined ? null : action}
    </div>
  );
}

/**
 * A load error of a page, a list or a panel (§4.7): the notice row in the error tone (a 3px
 * --hazard holder, the octagon icon, the words in --text), with an optional "Try again". The words
 * are the alert; the row is a card in the page (`data-place="page"`, shell.css), not a banner.
 */
export function ErrorNotice({
  text,
  onRetry,
}: {
  readonly text: ReactNode;
  readonly onRetry?: (() => void) | undefined;
}) {
  return (
    <div className="fd-notice fd-notice-row" data-tone="error" data-place="page">
      <Icon of={OctagonAlert} size={14} />
      <span className="fd-notice-text" role="alert">
        {text}
      </span>
      {onRetry === undefined ? null : (
        <button type="button" className="fd-ghost fd-sm" onClick={onRetry}>
          Try again
        </button>
      )}
    </div>
  );
}

/** The grid of a form: two columns from 900 px, one below. */
export function FormGrid({ children }: { readonly children: ReactNode }) {
  return <div className="fd-formgrid">{children}</div>;
}

/**
 * The error that a field shows: only after the field was touched or after a submit try, never on
 * a fresh form.
 */
export function fieldError(
  problem: string | null | undefined,
  touched: boolean,
  tried: boolean,
): string | null {
  return problem !== null && problem !== undefined && (touched || tried) ? problem : null;
}

/**
 * One field: the label above the control, the hint under it. An error replaces the hint.
 * `group` is for a control made of several parts (a folder picker): the name is a group name,
 * not a `<label>` that wraps buttons.
 */
export function Field({
  label,
  hint,
  error,
  span,
  group = false,
  layout,
  onBlur,
  children,
}: {
  readonly label: string;
  readonly hint?: ReactNode;
  readonly error?: string | null | undefined;
  readonly span?: 2;
  readonly group?: boolean;
  /** `row`: the label in a 200px column at the left of its control (§4.7, the settings rows). */
  readonly layout?: 'row';
  /** Fired when the focus leaves the field (the form marks it touched). */
  readonly onBlur?: () => void;
  readonly children: ReactNode;
}) {
  const id = useId();
  const failed = error !== null && error !== undefined && error !== '';
  const note = failed ? (
    <span className="fd-fielderror" role="alert">
      {error}
    </span>
  ) : hint === undefined ? null : (
    <span className="fd-fieldhint">{hint}</span>
  );
  // Only a focus that leaves the whole field counts: a move between its parts does not.
  const blur =
    onBlur === undefined
      ? undefined
      : (e: React.FocusEvent<HTMLDivElement>) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) onBlur();
        };
  return (
    // The blur handler only listens: a focus leaving any control of the field marks it touched.
    <div
      className="fd-fieldwrap"
      data-layout={layout}
      data-span={span}
      data-invalid={failed ? 'true' : undefined}
      role={group ? 'group' : undefined}
      aria-labelledby={group ? id : undefined}
      onBlur={blur}
    >
      {group ? (
        <>
          <span id={id} className="fd-fieldlabel">
            {label}
          </span>
          {children}
        </>
      ) : (
        // biome-ignore lint/a11y/noLabelWithoutControl: the control is the child of the field
        <label className="fd-fieldcontrol">
          <span className="fd-fieldlabel">{label}</span>
          {children}
        </label>
      )}
      {note}
    </div>
  );
}

/** The bottom bar of a form panel: Cancel, then the primary submit, aligned right. */
export function FormActions({ children }: { readonly children: ReactNode }) {
  return <div className="fd-formactions">{children}</div>;
}
