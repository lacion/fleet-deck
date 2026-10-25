// The quiet loading line of every page, list and panel (DESIGN-SYSTEM §4.7, base.css `.fd-loading`):
// a small turning ring in the in-progress blue, then the words in --dim. Reduce motion stops the
// ring (base.css).
export function Loading({ text }: { readonly text: string }) {
  return (
    <p className="fd-note fd-loading" role="status">
      {text}
    </p>
  );
}
