// What a screen shows when the selected app has nothing on this side, with the fix.
import type { ReactNode } from "react";
import { Button } from "@cloudflare/kumo";
import { useScope, useWindow } from "@/app/window";
import { ErrorState, Page, Section, EmptyState, TableSkeleton } from "@/components/ui-bits";
import { clean } from "@/lib/clean";
import { windowSeconds } from "@/lib/domain";

/** A wider look, for when nothing shows: one step to 24h, because errors are rare. */
export function WidenButton() {
  const { w, windows, setW, cycle } = useWindow();
  if (windowSeconds(w) < 86400 && windows.includes("24h")) return <Button variant="secondary" onClick={() => setW("24h")}>Show last 24h</Button>;
  if (w === windows[windows.length - 1]) return null;
  return <Button variant="secondary" onClick={() => cycle(1)}>Widen time window</Button>;
}

export function NoTelemetry() {
  const { app, setApp, warnings } = useScope();
  const { w } = useWindow();
  const name = clean(app);
  return (
    <EmptyState
      title={`${name} has no telemetry in the last ${w}`}
      description={warnings.length ? `Telemetry could not be read: ${warnings.join("; ")}` : "Widen the time window, or set OTEL_SERVICE_NAME to this name in the program. This prints the settings:"}
      commandLine={`mallard setup ${name}`}
      action={<div className="flex gap-2"><WidenButton /><Button variant="ghost" onClick={() => setApp("")}>Show all apps</Button></div>}
    />
  );
}

export function NoErrors() {
  const { app, setApp } = useScope();
  const name = clean(app);
  return (
    <EmptyState
      title={`${name} has no error tracking`}
      description="No project has this name. This creates one and prints its SENTRY_DSN for the program:"
      commandLine={`mallard setup ${name}`}
      action={<Button variant="ghost" onClick={() => setApp("")}>Show all apps</Button>}
    />
  );
}

/** A service in the filter that is not one of the app's: nothing can match. */
export function OutsideApp({ service, onClear }: { service: string; onClear: () => void }) {
  const { app } = useScope();
  return (
    <EmptyState
      title={`service=${clean(service)} is not part of ${clean(app)}`}
      description="Clear the filter, or choose another app."
      action={<Button onClick={onClear}>Clear the filter</Button>}
    />
  );
}

/** A telemetry screen: only once the app's services are known, and only if it has any. */
export function TelemetryScope({ byId = false, children }: { byId?: boolean; children: ReactNode }) {
  const { app, services, loading, error } = useScope();
  // What is asked for by id does not depend on the app having services.
  if (!app || byId) return <>{children}</>;
  if (loading) return <Page><TableSkeleton /></Page>;
  if (error && !services?.length) return <Page><ErrorState error={error} title="Could not load the apps" /></Page>;
  if (!services?.length) return <Page><Section><NoTelemetry /></Section></Page>;
  return <>{children}</>;
}
