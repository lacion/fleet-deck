// Test fixtures of the installer repair on the Omarchy card (BG-2).
import type { OmarchyActions, OmarchyStatus } from './omarchy';
import type { InstallPlanView, RepairJournalView, RestorePlanView } from './omarchyRepair';

export const status = (
  extra: Partial<NonNullable<OmarchyStatus['install']>> = {},
): OmarchyStatus => ({
  omarchy: { present: true, version: '3.1.0' },
  install: {
    installed: true,
    files_ok: 5,
    files_changed: 0,
    files_missing: 0,
    plugin_enabled: true,
    hyprland_line: true,
    outdated: false,
    ...extra,
  },
  screensaver: null,
  provision: null,
  running: null,
});

export const plan = (): InstallPlanView => ({
  plan_id: 'a'.repeat(32),
  expires_at: 1_792_400_120_000,
  files: [
    {
      asset_id: 'fleet_plugin_widget',
      path: '/home/o/.config/omarchy/fleet/widget.lua',
      classification: 'unrecorded_conflict',
      replaceable: true,
    },
    {
      asset_id: 'fleet_bindings',
      path: '/home/o/.config/hypr/fleet-bindings.conf',
      classification: 'recorded_edit',
      replaceable: true,
    },
  ],
  blocked: null,
});

export const restorePlan = (): RestorePlanView => ({
  plan_id: 'b'.repeat(32),
  expires_at: 1_792_400_120_000,
  repair_id: 'c'.repeat(32),
  files: [
    { asset_id: 'fleet_plugin_widget', path: '/h/widget.lua', action: 'copy_backup' },
    {
      asset_id: 'fleet_plugin_panel',
      path: '/h/panel.lua',
      action: 'move_aside_then_copy_backup',
    },
    { asset_id: 'fleet_bindings', path: '/h/fleet-bindings.conf', action: 'remove_published' },
    { asset_id: 'theme_template', path: '/h/theme.tpl', action: 'nothing' },
    { asset_id: 'hook_post_boot', path: '/h/post-boot', action: 'stop_restore_conflict' },
    { asset_id: 'hook_theme_set', path: '/h/theme-set', action: 'stop_repair_incomplete' },
  ],
});

export const journal = (extra: Partial<RepairJournalView> = {}): RepairJournalView => ({
  repair_id: 'c'.repeat(32),
  created_at: 1_792_400_000_000,
  state: 'complete',
  files: [
    { asset_id: 'fleet_bindings', state: 'published', backup_path: '/h/fleet-bindings.conf.kept' },
    { asset_id: 'fleet_plugin_widget', state: 'nothing_moved', backup_path: null },
  ],
  ...extra,
});

/** The three reads of the repair when a test does not use them: each one fails. */
export const unusedReads: Pick<OmarchyActions, 'installPlan' | 'repairs' | 'restorePlan'> = {
  installPlan: () => Promise.reject(new Error('down')),
  repairs: () => Promise.reject(new Error('down')),
  restorePlan: () => Promise.reject(new Error('down')),
};

/** Fake calls: only the ones that a test gives are real. */
export function fakeActions(over: Partial<OmarchyActions>): OmarchyActions {
  const down = async (): Promise<never> => {
    throw new Error('down');
  };
  return {
    status: down,
    act: down,
    installPlan: down,
    repairs: down,
    restorePlan: down,
    ...over,
  };
}
