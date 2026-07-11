import { useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Building2, Home, Mic, Settings as SettingsIcon, Users } from "lucide-react";

import logo from "@/../src-tauri/icons/128x128.png";

import {
  getSettings,
  listAsrModels,
  onMenuStartRecording,
  onMenuStopRecording,
  updateSettings,
} from "@/lib/ipc";
import { cn } from "@/lib/utils";
import { useSystemTheme } from "@/hooks/useSystemTheme";
import HomeView from "@/views/Home";
import RecordingView from "@/views/Recording";
import MeetingDetailView from "@/views/MeetingDetail";
import SettingsView from "@/views/Settings";
import PersonasView from "@/views/Personas";
import CustomersView from "@/views/Customers";
import CustomerDetailView from "@/views/CustomerDetail";
import OnboardingWizard from "@/components/OnboardingWizard";

export type Route =
  | { name: "home" }
  | { name: "recording" }
  | { name: "meeting"; meetingId: string }
  | { name: "settings" }
  | { name: "personas" }
  | { name: "customers" }
  | { name: "customer"; customerId: string }
  | { name: "onboarding" };

const NAV = [
  { route: { name: "home" } as Route, label: "Meetings", icon: Home },
  { route: { name: "recording" } as Route, label: "Record", icon: Mic },
  { route: { name: "customers" } as Route, label: "Customers", icon: Building2 },
  { route: { name: "settings" } as Route, label: "Settings", icon: SettingsIcon },
  { route: { name: "personas" } as Route, label: "Personas", icon: Users },
];

export default function App() {
  const [route, setRoute] = useState<Route | null>(null);
  const routeRef = useRef<Route | null>(null);
  routeRef.current = route;
  useSystemTheme();

  // First-launch check: show onboarding wizard for new users. Existing users
  // (who already have an ASR model downloaded) are auto-migrated.
  useEffect(() => {
    (async () => {
      const settings = await getSettings().catch(() => null);
      if (!settings) {
        setRoute({ name: "home" });
        return;
      }
      if (settings.onboardingComplete) {
        setRoute({ name: "home" });
        return;
      }
      // Existing user with models already downloaded — skip onboarding.
      const models = await listAsrModels().catch(() => []);
      if (models.some((m) => m.downloaded)) {
        await updateSettings({ ...settings, onboardingComplete: true }).catch(() => {});
        setRoute({ name: "home" });
        return;
      }
      setRoute({ name: "onboarding" });
    })();
  }, []);

  // Tray → RecordingView bridge: one-shot flags that tell RecordingView to
  // run its existing start()/stop() when the user uses the menu bar dropdown.
  const autoStartRef = useRef(false);
  const [autoStart, setAutoStart] = useState(false);
  const autoStopRef = useRef(false);
  const [autoStop, setAutoStop] = useState(false);

  // Tray → RecordingView bridge: one-shot flags that tell RecordingView to
  // run its existing start()/stop() when the user uses the menu bar dropdown.
  // Direct listen (not useTauriEvent) because these are no-payload signals.
  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    onMenuStartRecording(() => {
      if (routeRef.current?.name === "onboarding") return;
      setRoute({ name: "recording" });
      autoStartRef.current = true;
      setAutoStart(true);
    }).then((u) => {
      if (disposed) u();
      else unlisten = u;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    onMenuStopRecording(() => {
      if (routeRef.current?.name === "onboarding") return;
      setRoute({ name: "recording" });
      autoStopRef.current = true;
      setAutoStop(true);
    }).then((u) => {
      if (disposed) u();
      else unlisten = u;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  return (
    <div className="flex h-full">
      {/* Sidebar — hidden during onboarding */}
      {route && route.name !== "onboarding" && (
        <aside className="flex w-52 shrink-0 flex-col border-r bg-secondary/40">
          <div className="flex h-10 items-center gap-2 px-4">
            <img src={logo} alt="LilNotes" className="size-5 rounded-md" />
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
      )}

      {/* Main content */}
      <main className="min-w-0 flex-1 overflow-y-auto">
        {route?.name === "onboarding" && (
          <OnboardingWizard onComplete={() => setRoute({ name: "home" })} />
        )}
        {route?.name === "home" && <HomeView onNavigate={setRoute} />}
        {/* Always mounted so recording state (segments, meetingId, notes,
            live transcript listener) survives navigation away and back. */}
        <div className={cn("h-full", route?.name === "recording" ? "flex" : "hidden")}>
          <RecordingView
            active={route?.name === "recording"}
            onNavigate={setRoute}
            autoStart={autoStart}
            autoStop={autoStop}
            onAutoStartHandled={() => {
              autoStartRef.current = false;
              setAutoStart(false);
            }}
            onAutoStopHandled={() => {
              autoStopRef.current = false;
              setAutoStop(false);
            }}
          />
        </div>
        {route?.name === "meeting" && (
          <MeetingDetailView meetingId={route.meetingId} onNavigate={setRoute} />
        )}
        {route?.name === "settings" && <SettingsView />}
        {route?.name === "personas" && <PersonasView />}
        {route?.name === "customers" && <CustomersView onNavigate={setRoute} />}
        {route?.name === "customer" && (
          <CustomerDetailView customerId={route.customerId} onNavigate={setRoute} />
        )}
      </main>
    </div>
  );
}
