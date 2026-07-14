import { useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { Building2, Home, Mic, Settings as SettingsIcon, Users } from "lucide-react";

import logo from "@/../src-tauri/icons/128x128.png";

import {
  dbExists,
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
import { TooltipProvider } from "@/components/ui/tooltip";

export type Route =
  | { name: "home" }
  | { name: "recording" }
  | {
      name: "meeting";
      meetingId: string;
      autoDiarize?: boolean;
      /** Customer id of the page this meeting was opened from, so the back
       * button can return there instead of the Meetings list. Set only by
       * CustomerDetail's meeting-row clicks. */
      fromCustomerId?: string;
    }
  | { name: "settings" }
  | { name: "personas" }
  | { name: "customers" }
  | {
      name: "customer";
      customerId: string;
      /** Scroll to the Meetings section on mount. Set by MeetingDetail's
       * back button when returning from a meeting opened from this customer. */
      focusMeetings?: boolean;
    }
  | { name: "onboarding" };

const NAV = [
  { route: { name: "home" } as Route, label: "Meetings", icon: Home },
  { route: { name: "recording" } as Route, label: "Record", icon: Mic },
  { route: { name: "customers" } as Route, label: "Customers", icon: Building2 },
  { route: { name: "personas" } as Route, label: "Personas", icon: Users },
  { route: { name: "settings" } as Route, label: "Settings", icon: SettingsIcon },
];

export default function App() {
  const [route, setRoute] = useState<Route | null>(null);
  const routeRef = useRef<Route | null>(null);
  routeRef.current = route;
  useSystemTheme();

  // First-launch check: show onboarding wizard for new users. Existing users
  // (who already have an ASR model downloaded) are auto-migrated. We check
  // `dbExists()` first — new users route to onboarding without touching the
  // DB (and thus without triggering the Keychain prompt).
  useEffect(() => {
    (async () => {
      const exists = await dbExists().catch(() => false);
      if (!exists) {
        setRoute({ name: "onboarding" });
        return;
      }
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
    <TooltipProvider delayDuration={500}>
      <div className="flex h-full">
        {/* Sidebar — hidden during onboarding */}
        {route && route.name !== "onboarding" && (
          <aside className="flex w-52 shrink-0 flex-col border-r bg-secondary/40">
            {/* Brand sits below the traffic-light zone (pt-8 ≈ 32px clears the
              ~28px-tall lights) so it can use the full sidebar width as a
              heading for the nav items, rather than crowding next to them.
              The whole strip stays a drag region — the empty top padding is
              draggable too. */}
            <div data-tauri-drag-region className="flex items-center gap-2.5 px-4 pt-9 pb-2">
              <img src={logo} alt="LilNotes" className="size-8 rounded-lg" />
              <span className="text-base font-semibold tracking-tight">LilNotes</span>
            </div>
            <nav className="flex flex-1 flex-col gap-1 p-2">
              {NAV.filter(({ route: r }) => r.name !== "settings").map(
                ({ route: r, label, icon: Icon }) => (
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
                ),
              )}
              <div className="mt-auto">
                {NAV.filter(({ route: r }) => r.name === "settings").map(
                  ({ route: r, label, icon: Icon }) => (
                    <button
                      key={r.name}
                      onClick={() => setRoute(r)}
                      className={cn(
                        "flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground",
                        route.name === r.name && "bg-accent text-accent-foreground",
                      )}
                    >
                      <Icon className="size-4" />
                      {label}
                    </button>
                  ),
                )}
              </div>
            </nav>
          </aside>
        )}

        {/* Main content — drag row above the scrollable area so the window
          stays draggable from the top without a floating fixed overlay. */}
        <div className="flex min-w-0 flex-1 flex-col">
          <div data-tauri-drag-region className="h-10 shrink-0" />
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
              <MeetingDetailView
                meetingId={route.meetingId}
                autoDiarize={route.autoDiarize}
                fromCustomerId={route.fromCustomerId}
                onNavigate={setRoute}
              />
            )}
            {route?.name === "settings" && <SettingsView />}
            {route?.name === "personas" && <PersonasView />}
            {route?.name === "customers" && <CustomersView onNavigate={setRoute} />}
            {route?.name === "customer" && (
              <CustomerDetailView
                customerId={route.customerId}
                focusMeetings={route.focusMeetings}
                onNavigate={setRoute}
              />
            )}
          </main>
        </div>
      </div>
    </TooltipProvider>
  );
}
