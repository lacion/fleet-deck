import { Icon, type LucideIcon, Monitor, Moon, Rows3, Sun } from '../ui/icons';
import { useDensity } from '../ui/density';
import { type ThemeChoice, useTheme } from '../ui/theme';

const THEMES: readonly {
  readonly choice: ThemeChoice;
  readonly label: string;
  readonly icon: LucideIcon;
}[] = [
  { choice: 'light', label: 'Light theme', icon: Sun },
  { choice: 'dark', label: 'Dark theme', icon: Moon },
  { choice: 'system', label: 'System theme', icon: Monitor },
];

/** The theme choice and the compact cards switch (sidebar footer and the More page). */
export function PrefsRow() {
  const [theme, setTheme] = useTheme();
  const [density, setDensity] = useDensity();
  const compact = density === 'compact';
  return (
    <div className="fd-sidebar-prefs">
      <div className="fd-segmented" role="group" aria-label="Theme">
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
            <Icon of={t.icon} size={14} />
          </button>
        ))}
      </div>
      <button
        type="button"
        className="fd-iconbtn fd-sm"
        aria-pressed={compact}
        aria-label="Compact cards"
        title="Compact cards"
        onClick={() => setDensity(compact ? 'cozy' : 'compact')}
      >
        <Icon of={Rows3} size={14} />
      </button>
    </div>
  );
}
