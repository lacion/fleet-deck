import { useState } from 'react';
import { useIntegrations } from '../integrations/context';
import { useSettings } from '../settings/context';
import { FolderGit2 } from '../integrations/icons';
import {
  Activity,
  ChartColumn,
  ChevronRight,
  CircleHelp,
  Eye,
  FileText,
  Folder,
  Icon,
  LogOut,
  type LucideIcon,
  Paperclip,
  Search,
  Server,
  Settings,
  Smartphone,
  SquareTerminal,
  Sun,
  Users,
  Workflow,
} from '../ui/icons';
import { canShare, usePeople } from '../sharing/context';
import type { StationView } from '../gen';
import type { Chrome } from './chrome';
import { DeviceLinkDialog } from './DeviceLinkDialog';
import { EmptyState, Page } from '../ui/page';
import { CompactSwitch, MotionSwitch, PrefsRow } from './Prefs';

type Props = {
  /** The stations of the snapshot (the offline badge, the Omarchy select of the Prefs row). */
  readonly stations?: readonly StationView[] | undefined;
  readonly terminalCount: number;
  readonly chrome?: Chrome | undefined;
  readonly onPalette?: (() => void) | undefined;
  readonly onHelp?: (() => void) | undefined;
};

/**
 * A row that opens another page: the icon, the name, a count when it has one, and a 16px
 * chevron at the right (a row that opens a dialog or holds a control has none).
 */
function PageLink({
  href,
  icon,
  label,
  count,
  alert,
}: {
  readonly href: string;
  readonly icon: LucideIcon;
  readonly label: string;
  readonly count?: number | undefined;
  /** A hazard badge with its word ("1 offline") in place of a count. */
  readonly alert?: string | undefined;
}) {
  return (
    <li>
      <a href={href}>
        <Icon of={icon} />
        {label}
        {count === undefined ? null : <span className="fd-count">{count}</span>}
        {alert === undefined ? null : (
          <span className="fd-badge" data-tone="hazard">
            {alert}
          </span>
        )}
        <span className="fd-more-chev">
          <Icon of={ChevronRight} />
        </span>
      </a>
    </li>
  );
}

/**
 * The phone Terminals page without a terminal (§4.7 empty state): what to do, and the way to the
 * stations, where a shell opens.
 */
export function TerminalsEmpty() {
  return (
    <Page title="Terminals" description="Shells that you open from a station or a session.">
      <EmptyState
        icon={SquareTerminal}
        title="No terminals open"
        body="Open a shell from a station or a session."
        action={
          <a href="#/stations" className="fd-btn fd-primary">
            Go to Stations
          </a>
        }
      />
    </Page>
  );
}

/** The phone's "More" page: the pages that have no place in the bottom bar. */
export function More({ stations = [], terminalCount, chrome, onPalette, onHelp }: Props) {
  const [addingDevice, setAddingDevice] = useState(false);
  const projectsLink = useIntegrations()?.actions.canAdmin === true;
  const settingsLink = useSettings() !== undefined;
  // R11.4: a guest sees the Watch page only; the owner also sees Shares.
  const people = usePeople();
  const guest = people.member !== null;
  // DL-172: the settings are the owner's login only.
  const settings = settingsLink && !guest;
  const tools = onPalette !== undefined || settings || onHelp !== undefined;
  const offline = stations.filter((station) => !station.reachable).length;
  const analytics = <PageLink href="#/analytics" icon={ChartColumn} label="Analytics" />;
  return (
    <Page title="More" className="fd-more">
      {/* The pages in the order of the desktop sidebar, then the tools (§4.2, §4.8). */}
      <nav aria-label="More pages" className="fd-more-nav">
        <ul className="fd-more-links">
          <PageLink href="#/watch" icon={Eye} label="Watch" />
          {guest ? analytics : null}
          {guest ? null : (
            <>
              <PageLink href="#/activity" icon={Activity} label="Activity" />
              {analytics}
              <PageLink
                href="#/stations"
                icon={Server}
                label="Stations"
                alert={offline > 0 ? `${offline} offline` : undefined}
              />
              <PageLink href="#/plans" icon={FileText} label="Plans" />
              <PageLink href="#/automations" icon={Workflow} label="Automations" />
              <PageLink href="#/artifacts" icon={Paperclip} label="Artifacts" />
              {projectsLink ? (
                <PageLink href="#/projects" icon={FolderGit2} label="Projects" />
              ) : null}
              <PageLink href="#/files" icon={Folder} label="Files" />
            </>
          )}
          {canShare(people) ? <PageLink href="#/shares" icon={Users} label="Shares" /> : null}
          {guest ? null : (
            <PageLink
              href="#/terminals"
              icon={SquareTerminal}
              label="Terminals"
              count={terminalCount > 0 ? terminalCount : undefined}
            />
          )}
        </ul>
        {tools ? (
          <ul className="fd-more-links">
            {onPalette === undefined ? null : (
              <li>
                <button type="button" onClick={onPalette}>
                  <Icon of={Search} />
                  Search sessions
                </button>
              </li>
            )}
            {settings ? <PageLink href="#/settings" icon={Settings} label="Settings" /> : null}
            {onHelp === undefined ? null : (
              <li>
                <button type="button" onClick={onHelp}>
                  <Icon of={CircleHelp} />
                  Help
                </button>
              </li>
            )}
          </ul>
        ) : null}
      </nav>
      {/* The last group: how the board looks, and the devices of this login. */}
      <ul className="fd-more-links">
        <li className="fd-more-row">
          <span className="fd-more-rowlabel">
            <Icon of={Sun} />
            Appearance
          </span>
          <PrefsRow stations={stations} getPalette={chrome?.getPalette} iconSize={16} />
        </li>
        <li>
          <CompactSwitch />
        </li>
        <li>
          <MotionSwitch />
        </li>
        {chrome?.addDevice === undefined ? null : (
          <li>
            <button type="button" onClick={() => setAddingDevice(true)}>
              <Icon of={Smartphone} />
              Add a device
            </button>
          </li>
        )}
      </ul>
      {addingDevice && chrome?.addDevice !== undefined ? (
        <DeviceLinkDialog load={chrome.addDevice} onClose={() => setAddingDevice(false)} />
      ) : null}
      {chrome === undefined ? null : (
        <button type="button" className="fd-ghost fd-sm" onClick={chrome.onLogout}>
          <Icon of={LogOut} size={14} /> Log out
        </button>
      )}
      {chrome?.version === undefined ? null : (
        <p className="fd-sidebar-version" title="Deck version">
          deck {chrome.version}
        </p>
      )}
    </Page>
  );
}
