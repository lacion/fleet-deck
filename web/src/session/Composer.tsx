import { type ClipboardEvent, useState } from 'react';
import type { ImageInput } from '../gen';
import type { InputKind } from './commands';
import { IMAGE_TYPES, imageFiles, imagesProblem, readImage, toDataUrl } from './images';

type Props = {
  readonly enabled: boolean;
  readonly working: boolean;
  readonly steer: boolean;
  readonly nativeQueue: boolean;
  readonly onSend: (kind: InputKind, text: string, images: readonly ImageInput[]) => void;
};

/** The message box of a session: text, Shift+Enter newlines and pasted images (M3 6-10). */
export function Composer(props: Props) {
  const [text, setText] = useState('');
  const [images, setImages] = useState<readonly ImageInput[]>([]);
  const [readError, setReadError] = useState<string | null>(null);
  const problem = readError ?? imagesProblem(images);
  const empty = text.trim() === '';
  const blocked = !props.enabled || empty || problem !== null;

  const send = (kind: InputKind) => {
    const value = text.trim();
    if (value === '' || imagesProblem(images) !== null) return;
    setText('');
    setImages([]);
    props.onSend(kind, value, images);
  };
  const add = async (files: File[]) => {
    if (files.length === 0) return;
    setReadError(null);
    const read = await Promise.all(files.map((f) => readImage(f).catch(() => null)));
    const ok = read.filter((i): i is ImageInput => i !== null);
    if (ok.length < read.length) setReadError('An image could not be read.');
    setImages((now) => [...now, ...ok]);
  };
  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = imageFiles(event.clipboardData.files);
    if (files.length === 0) return;
    event.preventDefault();
    void add(files);
  };

  return (
    <form
      className="fd-composer"
      aria-label="Composer"
      onSubmit={(event) => {
        event.preventDefault();
        send(props.working ? 'follow_up' : 'prompt');
      }}
    >
      <label htmlFor="fd-composer-text">Message</label>
      <textarea
        id="fd-composer-text"
        rows={3}
        value={text}
        disabled={!props.enabled}
        onChange={(event) => setText(event.target.value)}
        onPaste={onPaste}
      />
      {images.length > 0 ? (
        <ul className="fd-images" aria-label="Images">
          {images.map((image, n) => (
            <li key={n}>
              <img src={toDataUrl(image)} alt={`Pasted ${n + 1}`} />
              <button
                type="button"
                aria-label={`Remove image ${n + 1}`}
                onClick={() => {
                  setReadError(null);
                  setImages((now) => now.filter((_, k) => k !== n));
                }}
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      ) : null}
      {problem ? (
        <p role="alert" className="fd-note">
          {problem}
        </p>
      ) : null}
      <div className="fd-options">
        <label className="fd-attach">
          Add image
          <input
            type="file"
            accept={IMAGE_TYPES.join(',')}
            multiple
            disabled={!props.enabled}
            onChange={(event) => {
              void add(imageFiles(event.target.files ?? []));
              event.target.value = '';
            }}
          />
        </label>
        {props.working ? (
          <>
            {props.steer ? (
              <button type="button" disabled={blocked} onClick={() => send('steer')}>
                Steer now
              </button>
            ) : null}
            <button type="submit" disabled={blocked}>
              Queue follow-up
            </button>
            {props.nativeQueue ? null : (
              <p className="fd-note">The station holds a follow-up until the turn ends.</p>
            )}
          </>
        ) : (
          <button type="submit" disabled={blocked}>
            Send
          </button>
        )}
      </div>
    </form>
  );
}
