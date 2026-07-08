import type { Route } from "@/App";

interface Props {
  onNavigate: (route: Route) => void;
}

/**
 * Recording view.
 *
 * Milestone 2 will add: record/stop control, dual level meters (mic +
 * system), elapsed timer. Milestone 3 adds the live transcript pane.
 */
export default function RecordingView(_props: Props) {
  return (
    <div className="flex h-full items-center justify-center p-8">
      <p className="text-sm text-muted-foreground">
        Recording view — audio capture arrives in milestone 2.
      </p>
    </div>
  );
}
