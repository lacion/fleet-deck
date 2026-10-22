import { CirclePause, Icon, type LucideIcon, Monitor, Moon, Rows3, Sun } from '../ui/icons';
import { useDensity } from '../ui/density';
import type { StationView } from '../gen';
import { type ThemeChoice, useReduceMotion, useTheme } from '../ui/theme';
import { type GetPalette, OmarchyThemeSelect } from './OmarchyTheme';

const THEMES: readonly {
  readonly choice: ThemeChoice;
  readonly label: string;
  readonly icon: LucideIcon;
}[] = [
  { choice: 'light', label: 'Light theme', icon: Sun },
  { choice: 'dark', label: 'Dark theme', icon: Moon },
  { choice: 'system', label: 'System theme', icon: Monitor },
];

/** The theme choice and the compact cards toggle (sidebar footer and the More page). */
export function PrefsRow({
  stations = [],
  getPalette,
  density: withDensity = true,
  iconSize = 14,
}: {
  /** The stations of the snapshot. */
  readonly stations?: readonly StationView[] | undefined;
  /** Set for the owner's login: the row then has the Omarchy select. */
  readonly getPalette?: GetPalette | undefined;
  /** The compact cards toggle; the More page has a switch row of its own instead. */
  readonly density?: boolean;
  /** The icon size: 14 in the 24px sidebar segments, 16 in the 44px touch rows of More. */
  readonly iconSize?: number;
} = {}) {
  const [theme, setTheme] = useTheme();
  const [density, setDensity] = useDensity();
  const compact = density === 'compact';
  const [reduce, setReduce] = useReduceMotion();
  return (
    <div className="fd-sidebar-prefs">
      <div className="fd-seg" role="group" aria-label="Theme">
        {THEMES.map((t) => (
          <button
            key={t.choice}
            type="button"
            className="fd-iconbtn fd-sm"
            aria-pressed={theme === t.choice}
            aria-label={t.label}
            title={t.label}
            onClick={() => setTheme(t.choice)}
          >
            <Icon of={t.icon} size={iconSize} />
          </button>
        ))}
      </div>
      {/* The density is a plain icon toggle next to the theme track (its own meaning). */}
      {withDensity ? (
        <button
          type="button"
          className="fd-iconbtn fd-sm fd-sidebar-density"
          aria-pressed={compact}
          aria-label="Compact cards"
          title="Compact cards"
          onClick={() => setDensity(compact ? 'cozy' : 'compact')}
        >
          <Icon of={Rows3} size={iconSize} />
        </button>
      ) : null}
      {/* Reduce motion stops the needs-you glow and the working arc (WCAG 2.2.2). */}
      {withDensity ? (
        <button
          type="button"
          className="fd-iconbtn fd-sm fd-sidebar-motion"
          aria-pressed={reduce}
          aria-label="Reduce motion"
          title="Reduce motion"
          onClick={() => setReduce(!reduce)}
        >
          <Icon of={CirclePause} size={iconSize} />
        </button>
      ) : null}
      {getPalette === undefined ? null : (
        <OmarchyThemeSelect stations={stations} getPalette={getPalette} />
      )}
    </div>
  );
}

/** The compact cards switch of the phone More page: a row with its name and a switch. */
export function CompactSwitch() {
  const [density, setDensity] = useDensity();
  const compact = density === 'compact';
  return (
    <label className="fd-more-row fd-more-switch">
      <span className="fd-more-rowlabel">
        <Icon of={Rows3} />
        Compact cards
      </span>
      <input
        type="checkbox"
        role="switch"
        className="fd-switch"
        aria-checked={compact}
        checked={compact}
        onChange={(e) => setDensity(e.target.checked ? 'compact' : 'cozy')}
      />
    </label>
  );
}

/**
 * The Reduce motion switch of the More page (WCAG 2.2.2): it stops the needs-you glow and the
 * working arc on this browser, also without the OS setting.
 */
export function MotionSwitch() {
  const [reduce, setReduce] = useReduceMotion();
  return (
    <label className="fd-more-row fd-more-switch">
      <span className="fd-more-rowlabel">
        <Icon of={CirclePause} />
        Reduce motion
      </span>
      <input
        type="checkbox"
        role="switch"
        className="fd-switch"
        aria-checked={reduce}
        checked={reduce}
        onChange={(e) => setReduce(e.target.checked)}
      />
    </label>
  );
}
