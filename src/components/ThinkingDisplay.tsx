import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Loader2 } from "lucide-react";

import { Markdown } from "@/components/SummaryPanel";

interface Props {
  /** Accumulated thinking tokens (null = none yet / cleared). */
  thinkingText: string | null;
  /** Seconds the model thought, once known; null while in progress. */
  thinkingDuration: number | null;
  /** True while a generation is running (card only shows mid-generation). */
  busy: boolean;
}

/**
 * Collapsible "train of thought" card shared by the per-meeting summary panel
 * and the customer rollup panel. Splits thinking into paragraph-level blocks
 * on double-newlines (Qwen3.5's natural thought boundary), auto-collapses the
 * previous block when a new one starts, and lets each block be expanded
 * individually. Mounted fresh per generation via `key`.
 */
export default function ThinkingDisplay({ thinkingText, thinkingDuration, busy }: Props) {
  const [thinkingExpanded, setThinkingExpanded] = useState(true);
  const [collapsedBlocks, setCollapsedBlocks] = useState<Set<number>>(new Set());
  const prevSegmentCount = useRef(0);
  const thinkingScrollRef = useRef<HTMLDivElement>(null);

  const toggleBlock = useCallback((i: number) => {
    setCollapsedBlocks((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }, []);

  const thinkingSegments = useMemo(() => {
    if (!thinkingText) return [];
    return thinkingText
      .split("\n\n")
      .map((s) => s.trim())
      .filter(Boolean)
      .reduce<string[]>((acc, seg) => {
        if (acc.length > 0 && seg.length < 100) {
          acc[acc.length - 1] += "\n" + seg;
        } else {
          acc.push(seg);
        }
        return acc;
      }, []);
  }, [thinkingText]);

  useEffect(() => {
    if (thinkingSegments.length > prevSegmentCount.current) {
      if (prevSegmentCount.current > 0) {
        setCollapsedBlocks((prev) => new Set(prev).add(prevSegmentCount.current - 1));
      }
      prevSegmentCount.current = thinkingSegments.length;
    }
  }, [thinkingSegments]);

  // Auto-scroll the thinking area as tokens arrive.
  useEffect(() => {
    if (thinkingText && thinkingExpanded) {
      requestAnimationFrame(() => {
        thinkingScrollRef.current?.scrollTo({
          top: thinkingScrollRef.current.scrollHeight,
        });
      });
    }
  }, [thinkingText, thinkingExpanded]);

  if (!busy || thinkingText === null) return null;

  return (
    <div className="border-b">
      <button
        onClick={() => setThinkingExpanded((v) => !v)}
        className="flex w-full items-center gap-1.5 px-4 py-2.5 text-xs font-medium text-muted-foreground"
      >
        {thinkingExpanded ? (
          <ChevronDown className="size-3" />
        ) : (
          <ChevronRight className="size-3" />
        )}
        {thinkingDuration !== null ? (
          <span>Thought for {thinkingDuration}s</span>
        ) : (
          <>
            <Loader2 className="size-3 animate-spin" />
            <span>Thinking…</span>
          </>
        )}
      </button>
      {thinkingExpanded && thinkingText && (
        <div ref={thinkingScrollRef} className="max-h-56 space-y-1 overflow-y-auto px-4 pb-3">
          {thinkingSegments.map((seg, i, arr) => {
            const isLast = i === arr.length - 1;
            const isActive = isLast && thinkingDuration === null;
            const isCollapsed = !isActive && collapsedBlocks.has(i);
            return (
              <div key={i} className="rounded-md bg-muted/20">
                <button
                  onClick={() => !isActive && toggleBlock(i)}
                  disabled={isActive}
                  className="flex w-full items-start gap-1.5 px-2.5 py-2 text-left"
                >
                  {isCollapsed ? (
                    <ChevronRight className="mt-0.5 size-3 shrink-0 text-muted-foreground/50" />
                  ) : (
                    <ChevronDown className="mt-0.5 size-3 shrink-0 text-muted-foreground/50" />
                  )}
                  {isCollapsed ? (
                    <span className="text-xs leading-relaxed text-muted-foreground/60 line-clamp-1">
                      {seg}
                    </span>
                  ) : (
                    <span className="text-xs font-medium text-muted-foreground">
                      {seg.split("\n")[0].slice(0, 80)}
                      {seg.length > 80 ? "…" : ""}
                    </span>
                  )}
                </button>
                {!isCollapsed && (
                  <div className="px-2.5 pb-2 pl-7">
                    <Markdown text={seg} className="text-xs text-muted-foreground/70" />
                    {isActive && <span className="animate-pulse">▌</span>}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
