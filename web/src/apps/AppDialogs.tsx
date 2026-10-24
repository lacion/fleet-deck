// B15 (design `docs/v2/design/mcp-apps.md` §4.8.3): the three dialogs that an app can ask for (a tool
// action, a message to the session, a link), and the small host that shows one at a time.
import type { ReactNode } from 'react';
import { Dialog } from '../integrations/Dialog';

export type AppDialog =
  | {
      readonly kind: 'tool';
      readonly server: string;
      readonly tool: string;
      readonly title: string | null;
      readonly args: unknown;
    }
  | { readonly kind: 'message'; readonly text: string }
  | { readonly kind: 'link'; readonly url: string };

/** The arguments as formatted JSON; the dialog scrolls it after 20 lines (apps.css). */
export function argsText(args: unknown): string {
  try {
    return JSON.stringify(args, null, 2) ?? 'null';
  } catch {
    return 'The arguments cannot be shown.';
  }
}

type Texts = {
  readonly title: string;
  readonly description: string | null;
  readonly yes: string;
  readonly body: ReactNode;
};

/** The words and the body of one dialog. */
export function dialogTexts(d: AppDialog): Texts {
  switch (d.kind) {
    case 'tool':
      return {
        title: 'Allow this app action?',
        description: `The app from ${d.server} wants to run ${d.title ?? d.tool}. It runs with the session's access to ${d.server}.`,
        yes: 'Allow',
        body: <pre className="fd-code fd-app-args">{argsText(d.args)}</pre>,
      };
    case 'message':
      return {
        title: 'Send this message to the session?',
        description: null,
        yes: 'Send',
        body: <blockquote className="fd-app-quote">{d.text}</blockquote>,
      };
    case 'link':
      return {
        title: 'Open this link?',
        description: null,
        yes: 'Open link',
        body: <p className="fd-app-url">{d.url}</p>,
      };
  }
}

/** One dialog. Cancel, Escape and the Close X answer `false`; the decision button answers `true`. */
export function AppDialogView(props: {
  readonly dialog: AppDialog;
  readonly onAnswer: (allowed: boolean) => void;
}) {
  const t = dialogTexts(props.dialog);
  return (
    <Dialog
      title={t.title}
      {...(t.description === null ? {} : { description: t.description })}
      onClose={() => props.onAnswer(false)}
    >
      <div className="fd-int-form">
        {t.body}
        <div className="fd-pane-row">
          <button type="button" onClick={() => props.onAnswer(false)}>
            Cancel
          </button>
          <button type="button" className="fd-decide" onClick={() => props.onAnswer(true)}>
            {t.yes}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

/** Shows one dialog at a time and gives the answer to the asker. */
export type DialogHost = {
  ask(dialog: AppDialog): Promise<boolean>;
  /** The person answered the dialog that shows now. */
  answer(allowed: boolean): void;
  /** Answers every dialog `false` (the frame went away). */
  dropAll(): void;
  current(): AppDialog | null;
};

export function createDialogHost(onChange: (current: AppDialog | null) => void): DialogHost {
  const queue: { dialog: AppDialog; resolve: (allowed: boolean) => void }[] = [];
  const show = () => onChange(queue[0]?.dialog ?? null);
  return {
    ask(dialog) {
      return new Promise<boolean>((resolve) => {
        queue.push({ dialog, resolve });
        if (queue.length === 1) show();
      });
    },
    answer(allowed) {
      queue.shift()?.resolve(allowed);
      show();
    },
    dropAll() {
      for (const q of queue.splice(0)) q.resolve(false);
      show();
    },
    current: () => queue[0]?.dialog ?? null,
  };
}
