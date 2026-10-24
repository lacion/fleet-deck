// "Flight Deck" §4.5: the properties panel at the right of a session. It shows only what the
// session summary (and the timeline that the pane already read) knows; a missing fact is absent.
import {
  Fragment,
  type ReactNode,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from 'react';
import { relativeAge } from '../app/sessions';
import { stationName } from '../board/fleet';
import { providerLabel } from '../board/lanes';
import { SlotMeter } from '../board/StationTraits';
import {
  costText,
  costTitle,
  dateText,
  dollars,
  rateMeters,
  SCOPE_TEXT,
  splitUsage,
  tokens,
  WARN_PERCENT,
} from '../board/usage';
import type { DeckSession, DiffSummary, StationView, Usage, UsageRecount } from '../gen';
import type { Freshness } from '../runtime/state';
import {
  addedText,
  compactCount,
  contextLabel,
  countText,
  homeShort,
  isRiskyMode,
  modeLabel,
  removedText,
  shortModel,
} from '../ui/format';
import { digestLines } from '../ui/generated';
import { ChevronRight, Icon, ShieldAlert } from '../ui/icons';
import { Menu } from '../ui/Menu';
import { sessionRef } from '../ui/localSet';
import { useNow } from '../ui/useNow';
import { loadSessionChanges, turnForPath } from './changedFiles';
import { CopyButton } from './CopyButton';
import { DigestList, GeneratedText, StatusFact } from './HeaderBadges';
import { FilePath } from './PathFit';
import { ModeLadder } from './SessionSettings';
import {
  type FileActions,
  type FinalizedTurn,
  setDiffTotal,
  useDiffPaths,
  useTurnPaths,
} from './workspace';

// The open state lives for the page: every session opens the panel the way the last one left it.
let open = true;
const listeners = new Set<() => void>();

/** The open state of the properties panel and its toggle (not stored: a reload opens it). */
export function usePropsOpen(): readonly [boolean, () => void] {
  const now = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => open,
    () => open,
  );
  return [
    now,
    () => {
      open = !open;
      for (const l of listeners) l();
    },
  ];
}

/** `five_hour` → `Claude 5-hour window`, `weekly` → `Claude weekly window`; no provider: `Weekly window`. */
export function scopeLabel(scope: string, provider: string | null): string {
  const words = SCOPE_TEXT[scope] ?? `${scope.replace(/_/g, ' ')} window`;
  if (provider === null) return `${words.charAt(0).toUpperCase()}${words.slice(1)}`;
  return `${provider} ${words}`;
}

/** The used slots of a station as ticks and "3 of 12", or "12 free" (`SlotMeter`, the notation of
 * its fleet lane and its station card); absent when the station does not say. */
function StationSlots({ station }: { readonly station: StationView }) {
  const max = station.info?.max_sessions ?? null;
  if (max === null || max <= 0) return null;
  return (
    <span className="fd-props-slots">
      {/* 16 ticks fill the 168px value column of the panel; more slots draw the neutral bar. */}
      <SlotMeter
        max={max}
        used={station.info?.live_sessions ?? 0}
        maxTicks={16}
        countClass="fd-props-aside"
      />
    </span>
  );
}

/**
 * The size of the session's changes: the loaded session diff. While that diff loads (`pending`: the
 * session has a spawn base and the board can read its files) the size is not known, so it is
 * absent: the sum of the turn diffs misses the files of a shared folder and would contradict the
 * diff a moment later. Without a session diff, the sum of the kept turns.
 */
export function changeTotal(
  loaded: DiffSummary | undefined,
  turns: readonly FinalizedTurn[],
  pending: boolean,
): DiffSummary | null {
  if (loaded !== undefined) return loaded;
  if (pending) return null;
  const kept = turns.filter((t) => !t.reverted && t.diff !== null);
  if (kept.length === 0) return null;
  return kept.reduce<DiffSummary>(
    (sum, t) => ({
      files_changed: sum.files_changed + (t.diff?.files_changed ?? 0),
      insertions: sum.insertions + (t.diff?.insertions ?? 0),
      deletions: sum.deletions + (t.diff?.deletions ?? 0),
    }),
    { files_changed: 0, insertions: 0, deletions: 0 },
  );
}

/**
 * One fact; `extra` is a second `<dd>` that takes the whole width of the panel under it. A row
 * with no value (the generated summary: its list is the `extra`) has no empty `<dd>`.
 */
function Row({
  label,
  children,
  extra,
}: {
  readonly label: ReactNode;
  readonly children?: ReactNode;
  readonly extra?: ReactNode;
}) {
  return (
    <div className="fd-props-row">
      <dt>{label}</dt>
      {children === undefined ? null : <dd>{children}</dd>}
      {extra}
    </div>
  );
}

/**
 * A path that breaks only after a `/`: each folder name (with its slash) is one unbreakable piece,
 * so a line never ends inside a name (not even after a hyphen of `fleet-deck-worktrees`).
 */
function Breakable({ path }: { readonly path: string }) {
  const parts = path.split('/');
  return (
    <>
      {parts.map((part, i) => {
        const last = i === parts.length - 1;
        return (
          // The parts of one path never move: the position is stable.
          <Fragment key={i}>
            <span className="fd-props-seg">{last ? part : `${part}/`}</span>
            {last ? null : <wbr />}
          </Fragment>
        );
      })}
    </>
  );
}

type Props = {
  readonly session: DeckSession;
  readonly stations: readonly StationView[];
  readonly turns: readonly FinalizedTurn[];
  /** The size of the session's changes (`changeTotal`, the same total as the Changes tab label). */
  readonly total: DiffSummary | null;
  /** The subagents and the background commands of the session, and the running count of the
      Agents bar (both kinds): the row and the summary of the bar show the same numbers. */
  readonly agents: number;
  readonly commands?: number;
  readonly runningAgents: number;
  /** A changed file opens the Changes tab on that file. */
  readonly onOpenChange: (path: string) => void;
  /** "+N more files" opens the Changes tab. */
  readonly onOpenAll?: (() => void) | undefined;
  /** The board connection: a stale board does not say that the station is online. */
  readonly freshness?: Freshness | undefined;
  /** The fleet-control fact of a coordinator session (a link that opens its answers). */
  readonly fleet?: ReactNode;
  /** The mode can be set: the Mode row is a menu with the mode ladder; absent: a plain fact. */
  readonly mode?: ModeControl | undefined;
};

/** The modes of the session and the call that sets one (the ladder of the Mode row). */
export type ModeControl = {
  readonly modes: readonly string[];
  readonly enabled: boolean;
  readonly onSet: (mode: string) => void;
};

/** The title of the token count: the input that the prompt cache did not hold; cache parts are other counts. */
export const NEW_INPUT_TITLE =
  'Input that the prompt cache did not hold. Cache reads and cache writes are separate counts.';

/** A part of a fact that keeps one line: its spaces do not break (the 280px row breaks at "·"). */
const keep = (text: string): string => text.replaceAll(' ', '\u00a0');

/**
 * One part of the token fact: the count and its words (`316K`, `output`). The first part of a
 * partial count has a `lead` ("at least").
 */
export type TokenPart = {
  readonly count: string;
  readonly word: string;
  readonly lead?: string;
};

/**
 * The token fact of the new-rule usage: four parts in this order, "new input", "cache read",
 * "cache write", "output" (a bare "682 in" next to a large cost read as a bug). A part whose count
 * is unknown is left out; "at least" leads a partial count. The words keep one line (no-break
 * spaces), so a narrow panel breaks the fact at the "·", never inside a part.
 */
export function tokensText(usage: Usage): TokenPart[] {
  const counts: readonly (readonly [number | null | undefined, string])[] = [
    [usage.input_tokens, 'new input'],
    [usage.cache_read_tokens, 'cache read'],
    [usage.cache_write_tokens, 'cache write'],
    [usage.output_tokens, 'output'],
  ];
  const parts: TokenPart[] = [];
  for (const [count, word] of counts) {
    if (count !== null && count !== undefined) {
      parts.push({ count: compactCount(count), word: keep(word) });
    }
  }
  const [first, ...rest] = parts;
  return first === undefined || usage.tokens_partial !== true
    ? parts
    : [{ ...first, lead: keep('at least ') }, ...rest];
}

/** The old count of a session as one fact: "1,200 input · 5 output · $1.20" (the known parts). */
export function oldCountText(old: Usage): string {
  return [
    old.input_tokens === null ? null : `${tokens(old.input_tokens)} input`,
    old.output_tokens === null ? null : `${tokens(old.output_tokens)} output`,
    old.cost_usd === null ? null : dollars(old.cost_usd),
  ]
    .filter((part) => part !== null)
    .join(' · ');
}

/** The title of the recount aside of the cost row (design §4.4 item 8). */
export function recountTitle(recount: UsageRecount): string {
  if (recount.source === 'none') {
    return `The provider has no record of this session. The total is unknown since ${dateText(recount.at)}.`;
  }
  const before =
    recount.previous.cost_usd === null ? 'unknown' : dollars(recount.previous.cost_usd);
  return `Recounted from the provider record on ${dateText(recount.at)}. A killed run and Codex sub-agent threads are not in that record, so the real total can be higher. The old count was ${before}.`;
}

/** The title of the "Old count" row (design §3.6 item 9). */
export function oldCountTitle(oldUntil: number | null): string {
  return oldUntil === null
    ? 'Fleet Deck counted this with an older rule that was wrong. It is not in the fleet totals. "Recount from provider records" on the Usage tab can replace it.'
    : `Fleet Deck counted this before ${dateText(oldUntil)} with an older rule that was wrong. It is not in the totals above or in the fleet totals.`;
}

/**
 * The token fact as the ops glance shows it: the numbers in --fuel, the words in --dim. Each part
 * is one unbreakable piece (`316K\u00a0output`).
 */
function TokensValue({ parts, title }: { readonly parts: TokenPart[]; readonly title?: string }) {
  return (
    <span className="fd-props-tokens" title={title}>
      {parts.map((part, i) => (
        <Fragment key={part.word}>
          {i === 0 ? null : ' · '}
          <span className="fd-props-tokpart">
            {part.lead}
            <span className="fd-props-fuel">{part.count}</span>
            {`\u00a0${part.word}`}
          </span>
        </Fragment>
      ))}
    </span>
  );
}

/** The mode of the session: a ghost menu summary that opens the mode ladder, or a plain fact. */
function ModeValue({ mode, control }: { readonly mode: string; readonly control?: ModeControl }) {
  const risky = isRiskyMode(mode);
  const value = (
    <>
      {risky ? <Icon of={ShieldAlert} size={13} /> : null}
      {modeLabel(mode)}
    </>
  );
  if (control === undefined) {
    return (
      <span
        className="fd-props-mode"
        data-tone={risky ? 'hazard' : undefined}
        title={`Permission mode: ${mode}`}
      >
        {value}
      </span>
    );
  }
  return (
    <span className="fd-props-mode fd-props-modemenu" data-tone={risky ? 'hazard' : undefined}>
      <Menu
        summary={value}
        summaryClass="fd-ghost fd-sm"
        variant="toggle"
        label={`Mode: ${modeLabel(mode)}`}
        title="Change the permission mode"
      >
        {() => (
          <ModeLadder
            modes={control.modes}
            current={mode}
            enabled={control.enabled}
            onSet={control.onSet}
          />
        )}
      </Menu>
    </span>
  );
}

/**
 * The scroll body fade of the panel (§3, session.css): `data-scrolled` away from its top,
 * `data-more-below` while rows hide under its bottom edge.
 */
function markEdges(el: HTMLElement) {
  el.toggleAttribute('data-scrolled', el.scrollTop > 0);
  el.toggleAttribute('data-more-below', el.scrollHeight - el.scrollTop - el.clientHeight > 1);
}

/** Marks the edges of `ref` after each render, on a scroll, and when it or a list resizes. */
function useScrollEdges(ref: { readonly current: HTMLElement | null }) {
  useLayoutEffect(() => {
    if (ref.current) markEdges(ref.current);
  });
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(() => markEdges(el));
    observer.observe(el);
    for (const child of el.children) observer.observe(child);
    return () => observer.disconnect();
  }, []);
  return (event: { readonly currentTarget: HTMLElement }) => markEdges(event.currentTarget);
}

/** The changed files that the Changes row lists; the others are "+N more files". */
export const PROPS_FILES_MAX = 5;

/**
 * The first changed files of the loaded session diff: a press opens the Changes tab on one. `files`
 * is the count of the whole diff (a cut patch lists fewer paths): "+N more files" counts from it
 * and opens the Changes tab.
 */
export function ChangedList({
  paths,
  files = 0,
  onOpen,
  onOpenAll,
  opens,
}: {
  readonly paths: readonly string[] | undefined;
  readonly files?: number;
  readonly onOpen: (path: string) => void;
  readonly onOpenAll?: (() => void) | undefined;
  /** A file that no diff can open shows as plain text; absent: every file opens. */
  readonly opens?: ((path: string) => boolean) | undefined;
}) {
  if (paths === undefined || paths.length === 0) return null;
  const listed = paths.slice(0, PROPS_FILES_MAX);
  const more = Math.max(files, paths.length) - listed.length;
  const moreText = `+${more} more ${more === 1 ? 'file' : 'files'}`;
  return (
    <ul className="fd-props-files" aria-label="Changed files">
      {listed.map((path) => (
        <li key={path}>
          {opens === undefined || opens(path) ? (
            <button type="button" className="fd-ghost" title={path} onClick={() => onOpen(path)}>
              <code className="fd-props-file fd-dl-path">
                <FilePath path={path} />
              </code>
              <Icon of={ChevronRight} size={14} />
            </button>
          ) : (
            <code className="fd-props-file fd-dl-path" title={path}>
              <FilePath path={path} />
            </code>
          )}
        </li>
      ))}
      {more <= 0 ? null : (
        <li className="fd-props-more">
          {onOpenAll === undefined ? (
            moreText
          ) : (
            <button type="button" className="fd-link fd-props-link" onClick={() => onOpenAll()}>
              {moreText}
            </button>
          )}
        </li>
      )}
    </ul>
  );
}

/** `2 min` → `since 2 min`; a change under a minute ago: `just now` (one word on every surface). */
export function sinceText(age: string): string {
  return age === 'now' ? 'just now' : `since ${age}`;
}

/** The read of the session diff: it runs, it gave the diff, or it failed (no diff will come). */
export type ChangesRead = 'loading' | 'done' | 'failed';

/**
 * Reads the session diff when the session opens, and again after each finalized turn: its size
 * (the Changes row and the Changes tab label) and its changed files (the list of the Changes row)
 * are there on every tab and every layout (the Files and Changes tabs read it too). No file
 * service, or no spawn base (the station has no session diff then): nothing. Returns the state of
 * the read for this session: after a failed read the size falls back to the turn diffs.
 */
export function useSessionChanges(
  ref: string,
  files: FileActions | null | undefined,
  hasBase: boolean,
  turns: number,
): ChangesRead {
  // The actions object changes on every render of the parent; the effect keys on "has files".
  const latest = useRef(files);
  latest.current = files;
  const has = files != null && hasBase;
  const [read, setRead] = useState<{ readonly ref: string; readonly state: ChangesRead }>({
    ref,
    state: 'loading',
  });
  useEffect(() => {
    const current = latest.current;
    if (!has || !current) return undefined;
    let live = true;
    // A read of the full session diff replaces the paths, an empty result too.
    void loadSessionChanges(current).then((found) => {
      if (!live) return;
      if (found === null) {
        setRead({ ref, state: 'failed' });
        return;
      }
      setDiffTotal(ref, found.summary, found.all);
      setRead({ ref, state: 'done' });
    });
    return () => {
      live = false;
    };
  }, [has, ref, turns]);
  // Another session starts with its own read.
  return read.ref === ref ? read.state : 'loading';
}

/** The properties of one session: status, place, agent, changes, usage and agents. */
export function PropertiesPanel({
  session,
  stations,
  turns,
  total,
  agents,
  commands = 0,
  runningAgents,
  onOpenChange,
  onOpenAll,
  freshness = 'live',
  fleet,
  mode,
}: Props) {
  const s = session.summary;
  const now = useNow(undefined, 30000);
  const station = stations.find((st) => st.station_id === session.station_id);
  const up = freshness !== 'stale' && station?.reachable === true;
  const ref = sessionRef(session.station_id, s.session_id);
  const paths = useDiffPaths(ref);
  // Without a spawn base only a file that a loaded turn diff lists opens (that turn's diff).
  const turnPaths = useTurnPaths(ref);
  const hasBase = Boolean(s.base);
  const opens = (path: string) => hasBase || turnForPath(turns, turnPaths, path) !== undefined;
  const { current, old, oldUntil } = splitUsage(s);
  const tokenParts = current === null ? [] : tokensText(current);
  const cost = current === null ? null : costText(current);
  const recount = s.usage_recount;
  const meters = rateMeters([session], stations);
  const provider = s.provider === null ? null : providerLabel(s.provider);
  const context = contextLabel(s.model);
  const box = useRef<HTMLElement>(null);
  const onScroll = useScrollEdges(box);
  return (
    <aside className="fd-props" aria-label="Properties" ref={box} onScroll={onScroll}>
      <h3 className="fd-props-title">Properties</h3>
      <dl className="fd-props-list">
        <Row label="Status">
          <StatusFact session={session} />
          <span className="fd-props-aside">{sinceText(relativeAge(s.updated_at, now))}</span>
        </Row>
        <Row label="Station">
          <span className="fd-props-station">{stationName(session.station_id, stations)}</span>
          {station === undefined ? null : (
            <span className="fd-props-aside" data-up={up}>
              <span
                className="fd-dot"
                data-status={up ? 'online' : 'unreachable'}
                aria-hidden="true"
              />
              {up ? 'online' : 'offline'}
            </span>
          )}
          {station === undefined ? null : <StationSlots station={station} />}
        </Row>
        {s.branch ? (
          <Row label="Branch">
            <code className="fd-props-mono">{s.branch}</code>
          </Row>
        ) : null}
        {s.cwd ? (
          <Row label="Folder">
            <code className="fd-props-mono" data-path="" title={s.cwd}>
              <Breakable path={homeShort(s.cwd)} />
            </code>
          </Row>
        ) : null}
        {s.worktree && s.worktree !== s.cwd ? (
          <Row label="Worktree">
            <code className="fd-props-mono" data-path="" title={s.worktree}>
              <Breakable path={homeShort(s.worktree)} />
            </code>
          </Row>
        ) : null}
      </dl>
      <dl className="fd-props-list">
        {/* The context window is a fact of the model: a --faint aside, not a row of its own. */}
        <Row label="Model">
          <span title={s.model ?? 'default model'}>{shortModel(s.model)}</span>
          {context === null ? null : <span className="fd-props-aside">{context}</span>}
        </Row>
        {s.effort ? (
          <Row label="Effort">
            <span title="The reasoning effort that the provider reports">{s.effort}</span>
          </Row>
        ) : null}
        {s.provider === null ? null : <Row label="Provider">{providerLabel(s.provider)}</Row>}
        {s.mode ? (
          <Row label="Mode">
            <ModeValue mode={s.mode} {...(mode === undefined ? {} : { control: mode })} />
          </Row>
        ) : null}
      </dl>
      <dl className="fd-props-list">
        {total === null ? null : total.files_changed === 0 ? (
          <Row label="Changes">
            <span className="fd-props-aside">No file changes</span>
          </Row>
        ) : (
          <Row
            label="Changes"
            extra={
              paths === undefined || paths.length === 0 ? null : (
                <dd className="fd-props-filelist">
                  <ChangedList
                    paths={paths}
                    files={total.files_changed}
                    onOpen={onOpenChange}
                    onOpenAll={onOpenAll}
                    opens={opens}
                  />
                </dd>
              )
            }
          >
            {total.insertions > 0 ? (
              <span className="fd-add">{addedText(total.insertions)}</span>
            ) : null}
            {total.deletions > 0 ? (
              <span className="fd-del">{removedText(total.deletions)}</span>
            ) : null}
            <span className="fd-props-aside">
              {total.files_changed} {total.files_changed === 1 ? 'file' : 'files'}
            </span>
          </Row>
        )}
        {current === null ? (
          <Row label="Tokens">unknown</Row>
        ) : tokenParts.length === 0 ? null : (
          <Row label="Tokens">
            <TokensValue
              parts={tokenParts}
              {...(current.input_tokens === null ? {} : { title: NEW_INPUT_TITLE })}
            />
          </Row>
        )}
        {current === null || (cost === null && recount?.source !== 'none') ? null : (
          <Row label="At API prices">
            <span className="fd-props-fuel" title={costTitle(current)}>
              {cost ?? 'unknown'}
            </span>
            {recount === undefined ? null : (
              <span className="fd-props-aside" title={recountTitle(recount)}>
                {recount.source === 'none' ? 'no record' : 'recounted'}
              </span>
            )}
          </Row>
        )}
        {old === null ? null : (
          <Row label="Old count">
            <span className="fd-props-fuel" title={oldCountTitle(oldUntil)}>
              {oldCountText(old)}
            </span>
          </Row>
        )}
        {meters.map((m) =>
          m.usedPercent === null ? null : (
            <div key={m.key} className="fd-props-meter">
              <span className="fd-props-meter-label">
                <span>{scopeLabel(m.scope, provider)}</span>
                <span className="fd-props-fuel">{Math.round(m.usedPercent)}%</span>
              </span>
              <span
                className="fd-meter"
                data-level={m.usedPercent >= WARN_PERCENT ? 'high' : undefined}
                aria-hidden="true"
              >
                <span style={{ width: `${Math.min(100, Math.max(0, m.usedPercent))}%` }} />
              </span>
            </div>
          ),
        )}
        {/* One rule with the Agents bar: an Agents row and a Commands row, each with its count;
            what runs now (both kinds) is the aside of the first row. */}
        {agents > 0 || (commands === 0 && runningAgents > 0) ? (
          <Row label="Agents">
            {agents > 0 ? <span>{countText(agents)}</span> : null}
            {runningAgents > 0 ? (
              <span className="fd-props-aside">{`${countText(runningAgents)} running`}</span>
            ) : null}
          </Row>
        ) : null}
        {commands > 0 ? (
          <Row label="Commands">
            <span>{countText(commands)}</span>
            {agents === 0 && runningAgents > 0 ? (
              <span className="fd-props-aside">{`${countText(runningAgents)} running`}</span>
            ) : null}
          </Row>
        ) : null}
        {fleet == null ? null : <Row label="Fleet control">{fleet}</Row>}
      </dl>
      {/* The facts that the session menu shows while this panel is closed. */}
      <dl className="fd-props-list">
        {/* The sparkles mark goes with the label: the list starts right under it. */}
        {digestLines(s.digest).length === 0 ? null : (
          <Row
            label={
              <>
                Summary <GeneratedText />
              </>
            }
            extra={
              <dd className="fd-props-filelist fd-props-digest">
                <DigestList digest={s.digest} />
              </dd>
            }
          />
        )}
        <Row label="Session">
          <code className="fd-props-mono fd-props-sid" title={s.session_id}>
            {s.session_id}
          </code>
          <CopyButton text={s.session_id} label="Copy the session id" />
        </Row>
        <Row label="Events">{countText(s.session_seq)}</Row>
      </dl>
    </aside>
  );
}
