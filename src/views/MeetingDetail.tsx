import type { Route } from "@/App";

interface Props {
  meetingId: string;
  onNavigate: (route: Route) => void;
}

/**
 * Meeting detail view.
 *
 * Milestone 5 will add: full transcript with editable speaker names,
 * summary panel (streaming, milestone 6), regenerate button, export
 * (milestone 7).
 */
export default function MeetingDetailView({ meetingId }: Props) {
  return (
    <div className="flex h-full items-center justify-center p-8">
      <p className="text-sm text-muted-foreground">
        Meeting {meetingId} — detail view arrives in milestone 5.
      </p>
    </div>
  );
}
