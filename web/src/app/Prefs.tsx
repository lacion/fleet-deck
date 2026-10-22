import { useState } from 'react';
import type { StationView } from '../gen';
import { usePeople } from '../sharing/context';
import { useDensity } from '../ui/density';
import {
  CirclePause,
  Icon,
  LogOut,
  type LucideIcon,
  Monitor,
  Moon,
  Rows3,
  Smartphone,
  Sun,
} from '../ui/icons';
import { Menu } from '../ui/Menu';
import { type ThemeChoice, useReduceMotion, useTheme } from '../ui/theme';
import type { Chrome } from './chrome';
import { DeviceLinkDialog } from './DeviceLinkDialog';
import { accountName, Avatar } from './Layout';
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

/**
 * The theme choice (sidebar footer and the More page). Compact cards and Reduce motion are named
 * switches: in the account menu of the sidebar and in the rows of the More page.
 */
export function PrefsRow({
  stations = [],
  getPalette,
  iconSize = 14,
}: {
  /** The stations of the snapshot. */
  readonly stations?: readonly StationView[] | undefined;
  /** Set for the owner's login: the row then has the Omarchy select. */
  readonly getPalette?: GetPalette | undefined;
  /** The icon size: 14 in the 24px sidebar segments, 16 in the 44px touch rows of More. */
  readonly iconSize?: number;
} = {}) {
  const [theme, setTheme] = useTheme();
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
      {getPalette === undefined ? null : (
        <OmarchyThemeSelect stations={stations} getPalette={getPalette} />
      )}
    </div>
  );
}

/**
 * Where a switch row sits: a 44px row of the More page, or a 28px row of the account menu (§3
 * `.fd-menu-body`). The name is visible in both, so the switch needs no icon-only label.
 */
type Place = 'more' | 'menu';

function SwitchRow({
  place,
  icon,
  label,
  checked,
  onChange,
}: {
  readonly place: Place;
  readonly icon: LucideIcon;
  readonly label: string;
  readonly checked: boolean;
  readonly onChange: (checked: boolean) => void;
}) {
  return (
    <label className={place === 'menu' ? 'fd-menu-switch' : 'fd-more-row fd-more-switch'}>
      <span className={place === 'menu' ? 'fd-menu-switch-label' : 'fd-more-rowlabel'}>
        <Icon of={icon} size={place === 'menu' ? 14 : 16} />
        {label}
      </span>
      <input
        type="checkbox"
        role="switch"
        className="fd-switch"
        aria-checked={checked}
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
      />
    </label>
  );
}

/** The compact cards switch: a row of the More page or of the account menu. */
export function CompactSwitch({ place = 'more' }: { readonly place?: Place } = {}) {
  const [density, setDensity] = useDensity();
  return (
    <SwitchRow
      place={place}
      icon={Rows3}
      label="Compact cards"
      checked={density === 'compact'}
      onChange={(on) => setDensity(on ? 'compact' : 'cozy')}
    />
  );
}

/**
 * The Reduce motion switch (WCAG 2.2.2): it stops the needs-you glow and the working arc on this
 * browser, also without the OS setting. A row of the More page or of the account menu.
 */
export function MotionSwitch({ place = 'more' }: { readonly place?: Place } = {}) {
  const [reduce, setReduce] = useReduceMotion();
  return (
    <SwitchRow
      place={place}
      icon={CirclePause}
      label="Reduce motion"
      checked={reduce}
      onChange={setReduce}
    />
  );
}

/**
 * The account menu of the sidebar footer (§4.2 item 5): who is signed in, the named switches of
 * this browser (Compact cards, Reduce motion), Add a device, then Log out after a rule.
 */
export function AccountMenu({ chrome }: { readonly chrome: Chrome }) {
  const people = usePeople();
  const [addingDevice, setAddingDevice] = useState(false);
  const addDevice = chrome.addDevice;
  return (
    <div className="fd-sidebar-account">
      <Menu summary={<Avatar />} summaryClass="fd-iconbtn" label="Account">
        {(close) => (
          <>
            <p className="fd-menu-text fd-account-who">
              Signed in as <span className="fd-account-name">{accountName(people)}</span>
            </p>
            <div className="fd-menu-sep" />
            <CompactSwitch place="menu" />
            <MotionSwitch place="menu" />
            {addDevice === undefined ? null : (
              <>
                <div className="fd-menu-sep" />
                <button
                  type="button"
                  onClick={() => {
                    close();
                    setAddingDevice(true);
                  }}
                >
                  <Icon of={Smartphone} size={14} /> Add a device
                </button>
              </>
            )}
            <div className="fd-menu-sep" />
            <button
              type="button"
              onClick={() => {
                close();
                chrome.onLogout();
              }}
            >
              <Icon of={LogOut} size={14} /> Log out
            </button>
          </>
        )}
      </Menu>
      {addingDevice && addDevice !== undefined ? (
        <DeviceLinkDialog load={addDevice} onClose={() => setAddingDevice(false)} />
      ) : null}
    </div>
  );
}
