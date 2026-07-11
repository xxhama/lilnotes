import { memo, useRef } from "react";
import { Editor, rootCtx, defaultValueCtx } from "@milkdown/kit/core";
import { commonmark } from "@milkdown/kit/preset/commonmark";
import { listener, listenerCtx } from "@milkdown/kit/plugin/listener";
import { Milkdown, MilkdownProvider, useEditor } from "@milkdown/react";
import { activityPlugin } from "./activityPlugin";
import { headingMarkerPlugin } from "./headingMarkerPlugin";

type NotesEditorProps = {
  /** Initial markdown content. Only applied on mount; remount via `key` to load new content. */
  initialValue: string;
  /** Called with the full markdown string when the document changes (debounced by Milkdown ~200ms). */
  onChange?: (markdown: string) => void;
  /** Called on every keystroke / document change, with no debounce. Use for live
   *  status indicators and to reset per-keystroke autosave timers. */
  onActivity?: () => void;
};

function NotesEditorInner({ initialValue, onChange, onActivity }: NotesEditorProps) {
  // Keep the latest callbacks in refs so the editor config (created once) always
  // invokes the current functions without needing to recreate the editor.
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const onActivityRef = useRef(onActivity);
  onActivityRef.current = onActivity;

  useEditor(
    (root) =>
      Editor.make()
        .config((ctx) => {
          ctx.set(rootCtx, root);
          ctx.set(defaultValueCtx, initialValue);
          ctx.get(listenerCtx).markdownUpdated((_ctx, markdown) => {
            onChangeRef.current?.(markdown);
          });
        })
        .use(commonmark)
        .use(listener)
        .use(headingMarkerPlugin)
        .use(activityPlugin(() => onActivityRef.current)),
    // Seed once on mount. `initialValue` is intentionally NOT a dependency:
    // recreating the editor on every keystroke (where onChange flows back into
    // parent state and a new initialValue prop) would tear down the ProseMirror
    // view and drop the cursor. To load different content later, remount via the
    // `key` prop from the parent.
    [],
  );

  return (
    <div className="notes-editor-root">
      <Milkdown />
    </div>
  );
}

function NotesEditor(props: NotesEditorProps) {
  return (
    <MilkdownProvider>
      <NotesEditorInner {...props} />
    </MilkdownProvider>
  );
}

// Memoize so the ~4 Hz playback-driven re-renders of MeetingDetail (from the
// audio player's timeupdate) don't re-render the Milkdown editor, which is
// always mounted on the Review tab. Props are stable across those re-renders.
export default memo(NotesEditor);
