import { $prose } from "@milkdown/kit/utils";
import { Plugin, PluginKey } from "@milkdown/kit/prose/state";
import type { EditorState, Selection } from "@milkdown/kit/prose/state";
import { Decoration, DecorationSet } from "@milkdown/kit/prose/view";
import type { Node as ProseNode } from "@milkdown/kit/prose/model";

const headingMarkerKey = new PluginKey("lilnotes-heading-marker");

/**
 * Reveals the raw markdown heading markers (`#`, `##`, `###`…) while the cursor
 * is inside a heading — Obsidian-Live-Preview behavior, scoped to headings only.
 *
 * Implementation: a *node* decoration that adds a class to the active heading;
 * the markers themselves are rendered as CSS `::before` generated content (see
 * index.css). The `::before` is `position: absolute` (out of the inline flow)
 * with `padding-left` reserving the gutter, so the caret navigates the heading's
 * normal content flow and never anchors to the `#` box — including on empty
 * headings, where an in-flow marker would otherwise land the caret before the
 * `#` (a `::before`-on-empty-editable browser quirk).
 *
 * Removing a heading stays intuitive: the built-in commonmark keymap downgrades
 * the heading on Backspace at the start of the line (h3 → h2 → h1 → paragraph),
 * so the visible marker drops one level each press until it becomes plain text.
 */
export const headingMarkerPlugin = $prose(() => {
  return new Plugin({
    key: headingMarkerKey,
    state: {
      init(_config, state) {
        return build(state);
      },
      apply(tr, value, _oldState) {
        if (!tr.docChanged && !tr.selectionSet) return value;
        return build(tr);
      },
    },
    props: {
      decorations(state: EditorState) {
        return this.getState(state);
      },
    },
  });
});

// Both EditorState and Transaction expose `doc` and `selection`.
type DocSelection = { doc: ProseNode; selection: Selection };

function build(state: DocSelection): DecorationSet {
  const { $from, $to } = state.selection;
  // Only mark the heading the cursor/selection is currently inside.
  if ($from.parent.type.name !== "heading") return DecorationSet.empty;
  if ($from.parent !== $to.parent) return DecorationSet.empty;

  const node = $from.parent;
  // Position before the heading node (before its opening token) and after it.
  const start = $from.start($from.depth) - 1;
  const end = start + node.nodeSize;
  const deco = Decoration.node(start, end, { class: "milkdown-heading-marker" });
  return DecorationSet.create(state.doc, [deco]);
}
