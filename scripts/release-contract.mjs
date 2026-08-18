// Shared release metadata. CI, the publish gate, and the payload/version
// verifiers import these lists so adding a shipping path or manifest is one
// deliberate edit rather than several synchronized copies.

export const PLUGIN_PAYLOAD_PATHS = Object.freeze([
  'compatibility.json',
  'hooks/',
  'commands/',
  'skills/',
  'agents/',
  'scripts/hook-launcher.sh',
  'scripts/fleet-hook.mjs',
  'scripts/fleet-sessionstart.mjs',
  'scripts/fleet-watch.mjs',
  'bin/',
  'src/daemon/',
  'board/',
  '.claude-plugin/',
]);

export const VERSION_MANIFEST_PATHS = Object.freeze([
  'package.json',
  '.claude-plugin/plugin.json',
  '.claude-plugin/marketplace.json',
  'board/package.json',
]);

export function versionFromManifest(file, json) {
  return file === '.claude-plugin/marketplace.json' ? json?.plugins?.[0]?.version : json?.version;
}
