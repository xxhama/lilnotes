import type { SimpleIcon } from "simple-icons";

interface Props {
  icon: SimpleIcon;
  className?: string;
}

/**
 * Brand logo from Simple Icons (lucide ships no brand icons). Filled with
 * `currentColor` rather than the brand hex so it themes like a lucide icon;
 * sized by the parent (e.g. Button's `[&_svg]:size-4`) or `className`.
 */
export default function BrandIcon({ icon, className }: Props) {
  return (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" className={className}>
      <path d={icon.path} />
    </svg>
  );
}
