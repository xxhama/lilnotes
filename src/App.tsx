import { useState } from "react";
import { Home, Mic, Settings as SettingsIcon } from "lucide-react";

import { cn } from "@/lib/utils";
import HomeView from "@/views/Home";
import RecordingView from "@/views/Recording";
import MeetingDetailView from "@/views/MeetingDetail";
import SettingsView from "@/views/Settings";

export type Route =
  | { name: "home" }
  | { name: "recording" }
  | { name: "meeting"; meetingId: string }
  | { name: "settings" };

const NAV = [
  { route: { name: "home" } as Route, label: "Meetings", icon: Home },
  { route: { name: "recording" } as Route, label: "Record", icon: Mic },
  { route: { name: "settings" } as Route, label: "Settings", icon: SettingsIcon },
];

export default function App() {
  const [route, setRoute] = useState<Route>({ name: "home" });

  return (
    <div className="flex h-full">
      {/* Sidebar */}
      <aside className="flex w-52 shrink-0 flex-col border-r bg-secondary/40">
        <div className="flex h-12 items-center gap-2 px-4">
          <div className="size-5 rounded-md bg-primary" />
          <span className="text-sm font-semibold tracking-tight">LilNotes</span>
        </div>
        <nav className="flex flex-col gap-1 p-2">
          {NAV.map(({ route: r, label, icon: Icon }) => (
            <button
              key={r.name}
              onClick={() => setRoute(r)}
              className={cn(
                "flex items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground",
                route.name === r.name && "bg-accent text-accent-foreground",
              )}
            >
              <Icon className="size-4" />
              {label}
            </button>
          ))}
        </nav>
      </aside>

      {/* Main content */}
      <main className="min-w-0 flex-1 overflow-y-auto">
        {route.name === "home" && <HomeView onNavigate={setRoute} />}
        {route.name === "recording" && <RecordingView onNavigate={setRoute} />}
        {route.name === "meeting" && (
          <MeetingDetailView meetingId={route.meetingId} onNavigate={setRoute} />
        )}
        {route.name === "settings" && <SettingsView />}
      </main>
    </div>
  );
}
