// B11 (DL-412, DL-413): where the command menus open, the ranking, the rows and the picks.
import { describe, expect, test } from 'bun:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import type { EventEnvelope } from '../gen';
import { Composer } from './Composer';
import {
  type CommandList,
  commandList,
  commandMenu,
  matchCommand,
  pickCommand,
  rankNames,
} from './slash';

const list = (
  names: readonly string[] | null,
  provider: 'claude' | 'codex' = 'claude',
  skills: readonly string[] = [],
): CommandList => ({ provider, names, skills: new Set(skills) });

describe('matchCommand, Claude (DL-412)', () => {
  test('the first word opens the menu, also after leading whitespace', () => {
    expect(matchCommand('/', 1, 'claude')).toEqual({ trigger: '/', start: 0, end: 1, query: '' });
    expect(matchCommand('  /rev', 6, 'claude')).toMatchObject({ start: 2, end: 6, query: 'rev' });
    expect(matchCommand('/review fix', 4, 'claude')).toEqual({
      trigger: '/',
      start: 0,
      end: 7,
      query: 'rev',
    });
  });

  test('a slash in another word, on another line or with the caret outside the word: no menu', () => {
    expect(matchCommand('fix /review', 11, 'claude')).toBeNull();
    expect(matchCommand('a\n/x', 4, 'claude')).toBeNull();
    expect(matchCommand('/review fix', 11, 'claude')).toBeNull();
    expect(matchCommand('/review', 0, 'claude')).toBeNull();
    expect(matchCommand('hello', 5, 'claude')).toBeNull();
    expect(matchCommand('', 0, 'claude')).toBeNull();
  });

  test('a dollar word is no trigger for Claude', () => {
    expect(matchCommand('use $sk', 7, 'claude')).toBeNull();
  });
});

describe('matchCommand, Codex (DL-412)', () => {
  test('a dollar word at the caret opens the skill menu in any word', () => {
    expect(matchCommand('use $sk', 7, 'codex')).toEqual({
      trigger: '$',
      start: 4,
      end: 7,
      query: 'sk',
    });
    expect(matchCommand('$', 1, 'codex')).toMatchObject({ trigger: '$', query: '' });
    expect(matchCommand('use $sk now', 7, 'codex')).toMatchObject({ start: 4, end: 7 });
  });

  test('the slash of the first word still matches', () => {
    expect(matchCommand('/', 1, 'codex')).toMatchObject({ trigger: '/', query: '' });
  });

  test('a dollar inside a word is no trigger', () => {
    expect(matchCommand('mail me$x', 9, 'codex')).toBeNull();
    expect(matchCommand('use $sk', 4, 'codex')).toBeNull();
  });
});

describe('rankNames (DL-413)', () => {
  test('prefix, then part prefix, then contains; each group sorted', () => {
    const names = ['marketing-skills:ab-testing', 'abc', 'xab', 'compact'];
    expect(rankNames(names, 'ab')).toEqual(['abc', 'marketing-skills:ab-testing', 'xab']);
    expect(rankNames(names, 'AB')).toEqual(['abc', 'marketing-skills:ab-testing', 'xab']);
  });

  test('the empty query gives every name, sorted without regard to case', () => {
    expect(rankNames(['review', 'Init', 'compact'], '')).toEqual(['compact', 'Init', 'review']);
  });
});

describe('commandMenu (DL-413)', () => {
  const menuOf = (l: CommandList, text: string, provider: 'claude' | 'codex' = 'claude') => {
    const match = matchCommand(text, text.length, provider);
    if (match === null) throw new Error('no match');
    return commandMenu(l, match);
  };

  test('300 names give 200 rows and a status row for the rest', () => {
    const names = Array.from({ length: 300 }, (_, n) => `cmd${String(n).padStart(3, '0')}`);
    const menu = menuOf(list(names), '/');
    expect(menu?.label).toBe('Commands');
    expect(menu?.rows).toHaveLength(200);
    expect(menu?.status).toBe('100 more. Type more letters to filter.');
    expect(menuOf(list(names.slice(0, 200)), '/')?.status).toBeNull();
  });

  test('a Claude row is /name, with the badge for a skill', () => {
    const menu = menuOf(list(['compact', 'deploy'], 'claude', ['deploy']), '/');
    expect(menu?.rows).toEqual([
      { key: 'compact', text: '/compact' },
      { key: 'deploy', text: '/deploy', badge: 'skill' },
    ]);
    expect(menu?.status).toBeNull();
  });

  test('the status texts of a Claude slash word', () => {
    expect(menuOf(list(null), '/co')).toEqual({
      label: 'Commands',
      rows: [],
      status: 'No command list from the agent yet. The text goes to the agent as a prompt.',
    });
    expect(menuOf(list([]), '/co')?.status).toBe(
      'This session has no commands. The text goes to the agent as a prompt.',
    );
    expect(menuOf(list(['init']), '/zz')?.status).toBe(
      'No matching command. The text goes to the agent as a prompt.',
    );
  });

  test('a Codex slash word lists the skills as $name with the Codex hint', () => {
    const codex = list(['find-skills', 'deploy'], 'codex');
    const menu = menuOf(codex, '/');
    expect(menu?.label).toBe('Skills');
    expect(menu?.rows).toEqual([
      { key: 'deploy', text: '$deploy' },
      { key: 'find-skills', text: '$find-skills' },
    ]);
    expect(menu?.status).toBe(
      'Codex runs a skill that the prompt names with $. Codex terminal commands such as /compact do not run here.',
    );
  });

  test('the status texts of a Codex slash word', () => {
    expect(menuOf(list(null, 'codex'), '/')?.status).toBe(
      'No skill list from Codex yet. The text goes to Codex as a prompt.',
    );
    expect(menuOf(list([], 'codex'), '/')?.status).toBe(
      'Codex reports no skills for this folder. The text goes to Codex as a prompt.',
    );
    expect(menuOf(list(['deploy'], 'codex'), '/zz')?.status).toBe(
      'No matching skill. The text goes to Codex as a prompt.',
    );
  });

  test('a Codex dollar word opens a menu only with matching skills', () => {
    expect(menuOf(list(['deploy'], 'codex'), 'use $zz', 'codex')).toBeNull();
    expect(menuOf(list([], 'codex'), '$', 'codex')).toBeNull();
    expect(menuOf(list(null, 'codex'), '$', 'codex')).toBeNull();
    const menu = menuOf(list(['deploy'], 'codex'), 'use $de', 'codex');
    expect(menu?.rows).toEqual([{ key: 'deploy', text: '$deploy' }]);
    expect(menu?.status).toBeNull();
  });
});

describe('pickCommand (DL-412)', () => {
  test('a Claude pick keeps the rest of the text', () => {
    const match = matchCommand('/rev fix', 4, 'claude');
    if (match === null) throw new Error('no match');
    expect(pickCommand('/rev fix', match, 'review', 'claude')).toEqual({
      text: '/review fix',
      caret: 8,
    });
  });

  test('a pick at the end of the text adds one space', () => {
    const match = matchCommand('/rev', 4, 'claude');
    if (match === null) throw new Error('no match');
    expect(pickCommand('/rev', match, 'review', 'claude')).toEqual({
      text: '/review ',
      caret: 8,
    });
  });

  test('a Codex pick inserts $name, also from the slash word', () => {
    const slash = matchCommand('/sk', 3, 'codex');
    if (slash === null) throw new Error('no match');
    expect(pickCommand('/sk', slash, 'skill-a', 'codex')).toEqual({
      text: '$skill-a ',
      caret: 9,
    });
    const word = matchCommand('x $s', 4, 'codex');
    if (word === null) throw new Error('no match');
    expect(pickCommand('x $s', word, 'skill-a', 'codex')).toEqual({
      text: 'x $skill-a ',
      caret: 11,
    });
  });
});

describe('commandList (DL-174, DL-411)', () => {
  const at = (n: number, commands: string[]): EventEnvelope =>
    ({
      seq: n,
      event: { kind: 'session.slash_commands', commands, skills: [] },
    }) as unknown as EventEnvelope;

  test('the provider is codex only for a Codex session', () => {
    expect(commandList({ provider: 'codex' }, []).provider).toBe('codex');
    expect(commandList({ provider: 'claude' }, []).provider).toBe('claude');
    expect(commandList({ provider: null }, []).provider).toBe('claude');
  });

  test('the names are the sorted union of the commands and the skills', () => {
    const l = commandList(
      { provider: 'claude', slash_commands: ['review', 'compact'], skills: ['deploy', 'review'] },
      [],
    );
    expect(l.names).toEqual(['compact', 'deploy', 'review']);
    expect([...l.skills]).toEqual(['deploy', 'review']);
  });

  test('a Codex summary has the skills only', () => {
    const l = commandList({ provider: 'codex', slash_commands: [], skills: ['b', 'a'] }, []);
    expect(l.names).toEqual(['a', 'b']);
  });

  test('without a summary list the newest event gives the commands', () => {
    const l = commandList({ provider: 'claude' }, [at(1, ['old']), at(2, ['init'])]);
    expect(l.names).toEqual(['init']);
  });

  test('no summary list and no event: names is null', () => {
    expect(commandList({ provider: 'claude' }, []).names).toBeNull();
  });
});

describe('the composer renders the command menu (DL-413)', () => {
  /** A composer whose draft is `text`; the caret starts at the end of the draft. */
  const render = (text: string, commands: CommandList) => {
    const store = { getItem: () => text, setItem: () => {}, removeItem: () => {} };
    Object.defineProperty(globalThis, 'sessionStorage', { value: store, configurable: true });
    try {
      return renderToStaticMarkup(
        createElement(Composer, {
          enabled: true,
          working: false,
          steer: false,
          nativeQueue: true,
          onSend: () => {},
          draftKey: 'k',
          commands,
        }),
      );
    } finally {
      Reflect.deleteProperty(globalThis, 'sessionStorage');
    }
  };

  test('a Claude slash with 20 names renders 20 options', () => {
    const names = Array.from({ length: 20 }, (_, n) => `cmd${String(n).padStart(2, '0')}`);
    const html = render('/', list(names));
    expect(html).toContain('role="listbox" aria-label="Commands"');
    expect(html.match(/role="option"/g)).toHaveLength(20);
  });

  test('a Codex dollar word renders the listbox Skills', () => {
    const html = render('$', list(['deploy'], 'codex'));
    expect(html).toContain('role="listbox" aria-label="Skills"');
    expect(html).toContain('$deploy');
  });

  test('a slash word without a list renders the status row, not a note', () => {
    const html = render('/co', list(null));
    expect(html).toContain('No command list from the agent yet.');
    expect(html).toContain('aria-disabled="true"');
    expect(html).not.toContain('This session reports no commands.');
  });

  test('plain text renders no menu', () => {
    expect(render('hello', list(['init']))).not.toContain('role="listbox"');
  });
});
