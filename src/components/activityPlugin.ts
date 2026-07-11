import { $prose } from "@milkdown/kit/utils";
import { Plugin, PluginKey } from "@milkdown/kit/prose/state";
import type { EditorView } from "@milkdown/kit/prose/view";
import type { Node as ProseNode } from "@milkdown/kit/prose/model";

const activityKey = new PluginKey("lilnotes-activity");

/**
 * Fires `getActivity()` on every *document* change (not selection-only updates),
 * bypassing Milkdown's `markdownUpdated` listener which is debounced ~200ms and
 * therefore can't be used to reset a per-keystroke autosave timer or to flip a
 * "Modified" indicator the instant a key lands.
 *
 * Uses the view plugin's `update` hook (a safe place for side effects, called
 * after each transaction's DOM update) rather than `state.apply` (which must stay
 * pure). The previous doc is tracked so selection/cursor moves don't count.
 */
export const activityPlugin = (getActivity: () => (() => void) | undefined) =>
  $prose(() => {
    let prevDoc: ProseNode | null = null;
    return new Plugin({
      key: activityKey,
      view: () => ({
        update(view: EditorView) {
          const doc = view.state.doc;
          if (prevDoc && !prevDoc.eq(doc)) getActivity()?.();
          prevDoc = doc;
        },
      }),
    });
  });
