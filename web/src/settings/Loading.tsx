// The quiet loading line of a settings part (DESIGN-SYSTEM §4.7): a small turning ring in the
// in-progress blue, then the words in --dim. Reduce motion stops the ring (base.css).
export function Loading({ text }: { readonly text: string }) {
  return (
    <p className="fd-note fd-set-loading" role="status">
      {text}
    </p>
  );
}
