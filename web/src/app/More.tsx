import { useState } from 'react';
import { useIntegrations } from '../integrations/context';
import { useSettings } from '../settings/context';
import { FolderGit2 } from '../integrations/icons';
import {
  Eye,
  FileText,
  Folder,
  Icon,
  LogOut,
  MessagesSquare,
  Paperclip,
  Server,
  Settings,
  Smartphone,
  SquareTerminal,
  Users,
  Workflow,
} from '../ui/icons';
import { canShare, usePeople } from '../sharing/context';
import type { Chrome } from './chrome';
import { DeviceLinkDialog } from './DeviceLinkDialog';
import { Page } from '../ui/page';
import { PrefsRow } from './Prefs';

type Props = {
  readonly stationCount: number;
  readonly terminalCount: number;
  readonly chrome?: Chrome | undefined;
};

/** The phone's "More" page: the pages that have no place in the bottom bar. */
export function More({ stationCount, terminalCount, chrome }: Props) {
  const [addingDevice, setAddingDevice] = useState(false);
  const projectsLink = useIntegrations()?.actions.canAdmin === true;
  const settingsLink = useSettings() !== undefined;
  // R11.4: a guest sees the Watch page only; the owner also sees Shares.
  const people = usePeople();
  const guest = people.member !== null;
  return (
    <Page title="More" className="fd-more">
      <nav aria-label="More pages">
        <ul className="fd-more-links">
          <li>
            <a href="#/watch">
              <Icon of={Eye} />
              Watch
            </a>
          </li>
          {guest ? null : (
            <>
              <li>
                <a href="#/activity">
                  <Icon of={MessagesSquare} />
                  Activity
                </a>
              </li>
              <li>
                <a href="#/plans">
                  <Icon of={FileText} />
                  Plans
                </a>
              </li>
              <li>
                <a href="#/automations">
                  <Icon of={Workflow} />
                  Automations
                </a>
              </li>
              <li>
                <a href="#/artifacts">
                  <Icon of={Paperclip} />
                  Artifacts
                </a>
              </li>
              {projectsLink ? (
                <li>
                  <a href="#/projects">
                    <Icon of={FolderGit2} />
                    Projects
                  </a>
                </li>
              ) : null}
              <li>
                <a href="#/files">
                  <Icon of={Folder} />
                  Files
                </a>
              </li>
              <li>
                <a href="#/stations">
                  <Icon of={Server} />
                  Stations
                  <span className="fd-count">{stationCount}</span>
                </a>
              </li>
            </>
          )}
          {canShare(people) ? (
            <li>
              <a href="#/shares">
                <Icon of={Users} />
                Shares
              </a>
            </li>
          ) : null}
          {settingsLink && !guest ? (
            <li>
              <a href="#/settings">
                <Icon of={Settings} />
                Settings
              </a>
            </li>
          ) : null}
          {guest ? null : (
            <li>
              <a href="#/terminals">
                <Icon of={SquareTerminal} />
                Terminals
                <span className="fd-count">{terminalCount}</span>
              </a>
            </li>
          )}
        </ul>
      </nav>
      <PrefsRow />
      {chrome?.addDevice === undefined ? null : (
        <button type="button" className="fd-ghost fd-sm" onClick={() => setAddingDevice(true)}>
          <Icon of={Smartphone} size={14} /> Add a device
        </button>
      )}
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
