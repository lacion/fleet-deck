// Test-only ESM loader for tests/board-vite-proxy.test.mjs.
//
// WHY a loader: board/vite.config.js imports 'vite' and '@vitejs/plugin-react',
// which live in board/node_modules — a separate dependency tree the root
// `npm install` does not create. The proxy target config is plain data, so we
// stub the two imports with identity/no-op stand-ins and import the config
// directly. This pins the FLEETDECK_PORT behavior (BUG-004) without pulling
// the board's toolchain into the root test run.

const VITE_URL = 'fleetdeck-test:vite';
const REACT_URL = 'fleetdeck-test:vite-plugin-react';

export async function resolve(specifier, context, nextResolve) {
  // The test imports the config with a cache-busting ?query, so match the path
  // portion of parentURL, not the whole string.
  const fromConfig = context.parentURL?.split('?')[0].endsWith('/board/vite.config.js');
  if (specifier === 'vite' && fromConfig) {
    return { url: VITE_URL, shortCircuit: true };
  }
  if (specifier === '@vitejs/plugin-react' && fromConfig) {
    return { url: REACT_URL, shortCircuit: true };
  }
  return nextResolve(specifier, context);
}

export async function load(url, context, nextLoad) {
  if (url === VITE_URL) {
    return {
      format: 'module',
      shortCircuit: true,
      source: 'export const defineConfig = (config) => config;\n',
    };
  }
  if (url === REACT_URL) {
    return {
      format: 'module',
      shortCircuit: true,
      source: 'export default function react() { return { name: "react-stub" }; }\n',
    };
  }
  return nextLoad(url, context);
}
