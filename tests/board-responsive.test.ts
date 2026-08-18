import { readFileSync } from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import test from './helpers/harness-test.ts';

const css = readFileSync(path.resolve('board/src/app.css'), 'utf8');

function mobileRule(selector: string): string {
  const mobile = css.slice(css.lastIndexOf('@media (max-width: 720px)'));
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = new RegExp(`${escaped}\\s*\\{([^}]+)\\}`).exec(mobile);
  assert.ok(match, `missing mobile rule for ${selector}`);
  return match[1] ?? '';
}

function laptopRule(selector: string): string {
  const start = css.indexOf('@media (min-width: 721px) and (max-width: 1400px)');
  const end = css.indexOf('@media (max-width: 720px)', start);
  assert.notEqual(start, -1, 'missing laptop-width media query');
  const laptop = css.slice(start, end === -1 ? undefined : end);
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = new RegExp(`${escaped}\\s*\\{([^}]+)\\}`).exec(laptop);
  assert.ok(match, `missing laptop rule for ${selector}`);
  return match[1] ?? '';
}

test('phone layout removes the desktop lane and inbox width floors', () => {
  assert.match(mobileRule('.fd-main'), /flex-direction:\s*column/);
  assert.match(mobileRule('.fd-inbox'), /width:\s*100%/);
  assert.match(mobileRule('.fd-colheads, .fd-lane'), /min-width:\s*0/);
  assert.match(mobileRule('.fd-lanegrid'), /grid-template-columns:\s*minmax\(0,\s*1fr\)/);
});

test('phone header scrolls and large modals keep controls inside the viewport', () => {
  assert.match(mobileRule('.fd-header'), /overflow-x:\s*auto/);
  assert.match(mobileRule('.fd-compose'), /max-height:\s*calc\(100dvh\s*-\s*16px\)/);
  assert.match(mobileRule('.fd-fsviewer'), /width:\s*100%/);
  assert.match(mobileRule('.fd-fsbody'), /grid-template-columns:\s*minmax\(0,\s*1fr\)/);
});

test('laptop header keeps every action reachable without clipping', () => {
  assert.match(laptopRule('.fd-header'), /gap:\s*8px/);
  assert.match(laptopRule('.fd-header'), /overflow-x:\s*auto/);
  assert.match(laptopRule('.fd-header .fd-clock,\n  .fd-header .fd-fleetver'), /display:\s*none/);
});
