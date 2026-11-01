import { lazy, Suspense } from "react";
import { Banner, Loader } from "@cloudflare/kumo";
import { WarningCircleIcon } from "@phosphor-icons/react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { get, setUnauthorizedHandler, type Session } from "@/api/client";
import { ErrorBoundary } from "@/app/ErrorBoundary";
import { Login } from "@/app/Login";
import { Shell } from "@/app/Shell";
import { ScopeProvider } from "@/app/window";
import { TelemetryScope } from "@/components/scope-states";
import { IssuePage } from "@/features/errors/IssuePage";
import { ErrorsLanding, IssuesPage } from "@/features/errors/IssuesPage";
import { TraceErrorsPage } from "@/features/errors/TraceErrorsPage";
import { LogsPage } from "@/features/telemetry/LogsPage";
import { MetricsPage } from "@/features/telemetry/MetricsPage";
import { StatusPage } from "@/features/telemetry/StatusPage";
import { TracePage } from "@/features/telemetry/TracePage";
import { TracesPage } from "@/features/telemetry/TracesPage";

// ECharts is the biggest dependency: load it with the screens that draw lines.
const OverviewPage = lazy(() => import("@/features/telemetry/OverviewPage"));
const MetricPage = lazy(() => import("@/features/telemetry/MetricPage"));

/** Keeps what a link says about the app and the window. */
function Home() {
  const { search } = useLocation();
  return <Navigate to={{ pathname: "/telemetry/overview", search }} replace />;
}

function Centered({ children }: { children: React.ReactNode }) {
  return <div className="grid h-full min-h-40 place-items-center">{children}</div>;
}

export function App() {
  const client = useQueryClient();
  const { pathname } = useLocation();
  const session = useQuery({
    queryKey: ["session"],
    queryFn: ({ signal }) => get<Session>("/web/session", undefined, signal),
    retry: 1,
  });
  useEffect(() => {
    setUnauthorizedHandler(() => void client.invalidateQueries({ queryKey: ["session"] }));
  }, [client]);

  if (session.isPending) return <div className="grid h-dvh place-items-center"><Loader size="lg" /></div>;
  if (session.isError) {
    return (
      <div className="grid h-dvh place-items-center p-6">
        <Banner variant="error" icon={<WarningCircleIcon weight="fill" />} title="Cannot reach mallard web" description={String(session.error.message)} />
      </div>
    );
  }
  if (!session.data.authenticated) return <Login />;

  return (
    <ScopeProvider>
    <Shell session={session.data}>
      <ErrorBoundary resetKey={pathname}>
      <Suspense fallback={<Centered><Loader /></Centered>}>
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/errors" element={<ErrorsLanding />} />
          <Route path="/errors/:project" element={<IssuesPage />} />
          <Route path="/errors/:project/issues/:issueId" element={<IssuePage />} />
          <Route path="/errors-by-trace/:traceId" element={<TraceErrorsPage />} />
          <Route path="/telemetry" element={<Home />} />
          <Route path="/telemetry/overview" element={<OverviewPage />} />
          <Route path="/telemetry/traces" element={<TelemetryScope><TracesPage /></TelemetryScope>} />
          <Route path="/telemetry/traces/:traceId" element={<TracePage />} />
          <Route path="/telemetry/logs" element={<TelemetryScope><LogsPage /></TelemetryScope>} />
          <Route path="/telemetry/metrics" element={<TelemetryScope><MetricsPage /></TelemetryScope>} />
          <Route path="/telemetry/metrics/:name" element={<TelemetryScope><MetricPage /></TelemetryScope>} />
          <Route path="/telemetry/status" element={<StatusPage />} />
          <Route path="*" element={<Home />} />
        </Routes>
      </Suspense>
      </ErrorBoundary>
    </Shell>
    </ScopeProvider>
  );
}
