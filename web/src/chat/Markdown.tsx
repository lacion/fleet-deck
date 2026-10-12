// Safe Markdown for the chat (UI-PLAN §2.5): no raw HTML, links open in a new
// tab, images are never loaded and render as links.
import {
  type ComponentProps,
  Children,
  isValidElement,
  memo,
  type ReactNode,
  useEffect,
  useState,
} from 'react';
import ReactMarkdown, { type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Check, Copy, Icon } from '../ui/icons';

const COPIED_MS = 1500;
const PLUGINS = [remarkGfm];

/** The plain text of a rendered node tree (the code of a block). */
function textOf(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(textOf).join('');
  if (node && typeof node === 'object' && 'props' in node) {
    return textOf((node.props as { children?: ReactNode }).children);
  }
  return '';
}

/** The language of a fenced block: the `language-xxx` class of its `code` element. */
function languageOf(children: ReactNode): string | undefined {
  const first = Children.toArray(children)[0];
  if (!isValidElement(first)) return undefined;
  const { className } = first.props as { className?: string };
  return className?.match(/language-(\S+)/)?.[1];
}

function CodeBlock({ children }: { children?: ReactNode }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), COPIED_MS);
    return () => clearTimeout(timer);
  }, [copied]);
  const copy = () => {
    navigator.clipboard
      .writeText(textOf(children))
      .then(() => setCopied(true))
      .catch(() => setCopied(false));
  };
  return (
    <div className="fd-md-code">
      <div className="fd-md-code-head">
        <span className="fd-md-lang">{languageOf(children) ?? ''}</span>
        <button
          type="button"
          className="fd-iconbtn fd-sm fd-md-copy"
          aria-label={copied ? 'Copied' : 'Copy code'}
          title={copied ? 'Copied' : 'Copy code'}
          onClick={copy}
        >
          <Icon of={copied ? Check : Copy} size={14} />
        </button>
      </div>
      <pre>{children}</pre>
    </div>
  );
}

const components: Components = {
  a: ({ node: _node, children, href }: ComponentProps<'a'> & { node?: unknown }) => (
    <a href={href} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  ),
  img: ({ node: _node, src, alt }: ComponentProps<'img'> & { node?: unknown }) => {
    const label = alt || src || '';
    return src ? (
      <a href={src} target="_blank" rel="noopener noreferrer">
        {label}
      </a>
    ) : (
      <span>{label}</span>
    );
  },
  pre: ({ node: _node, children }: ComponentProps<'pre'> & { node?: unknown }) => (
    <CodeBlock>{children}</CodeBlock>
  ),
  table: ({ node: _node, children }: ComponentProps<'table'> & { node?: unknown }) => (
    <div className="fd-md-table">
      <table>{children}</table>
    </div>
  ),
};

export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div className="fd-md">
      <ReactMarkdown remarkPlugins={PLUGINS} skipHtml components={components}>
        {text}
      </ReactMarkdown>
    </div>
  );
});
