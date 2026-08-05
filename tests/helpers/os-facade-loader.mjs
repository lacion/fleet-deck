// Test-only module customization hook for network-refresh.test.mjs.
//
// WHY a customization hook instead of monkey-patching: os.networkInterfaces()
// cannot be reassigned once imported (Node caches the builtin binding), and a
// child-process loader would make an in-process HTTP assertion pointlessly
// indirect. The hook returns a facade whose networkInterfaces() reads a
// per-process interface list the test can swap — exactly the "the OS view
// changed under the daemon" condition BUG-129 fixes. Everything else is the
// real builtin, lazily delegated so the facade's own evaluation never touches
// the builtin binding before it exists.

const SELF = new URL('./os-facade-loader.mjs', import.meta.url).href;

const FACADE_SOURCE = [
  "import { createRequire } from 'node:module';",
  `const realOs = createRequire(${JSON.stringify(SELF)})('node:os');`,
  'let interfaces = Object.values(realOs.networkInterfaces()).flat();',
  'export function __setInterfaces(entries) { interfaces = entries; }',
  'export default new Proxy(realOs, {',
  '  get(target, prop) {',
  '    if (prop === \'networkInterfaces\') return () => ({ test: interfaces });',
  '    const value = target[prop];',
  "    return typeof value === 'function' ? value.bind(target) : value;",
  '  },',
  '  has(target, prop) {',
  // A Proxy that only overrides `get` would leak `networkInterfaces` into ESM
  // named-import detection — and an ESM named import reads through the REAL
  // builtin's own snapshot, not our proxy. Hiding it forces anything in the
  // tree to the default-export path we control.
  "    return prop === 'networkInterfaces' ? false : Reflect.has(target, prop);",
  '  },',
  '  ownKeys(target) {',
  "    return Reflect.ownKeys(target).filter(k => k !== 'networkInterfaces');",
  '  },',
  '});',
].join('\n');

export async function load(url, context, nextLoad) {
  if (url !== 'node:os') return nextLoad(url, context);
  return { format: 'module', shortCircuit: true, source: FACADE_SOURCE };
}
