// Deploy markers on charts (issue 0195). A deploy goes by version, and a version belongs to a
// project. A telemetry chart reaches it through the app: a project and a service with one slug
// (ADR 0027). With All apps, every deploy; an app with no project has no deploys to show.
import { useMemo } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { sentry } from "@/api/sentry";
import { useInterval } from "@/app/settings";
import { useScope } from "@/app/window";
import { deployMarks, type DeployMark } from "@/lib/deploys";

/** `project` for a page that has its own; otherwise the chosen app decides. */
export function useDeployMarks(w: string, project?: string): { marks: DeployMark[]; error: Error | null } {
  const scope = useScope();
  const refetchInterval = useInterval(30_000);
  const wanted = project ?? scope.project;
  const q = useQuery({
    queryKey: ["sentry", "deploys", wanted, w],
    enabled: !!project || !scope.app || !!scope.project,
    queryFn: ({ signal }) => sentry.deploys(wanted, w, signal),
    refetchInterval,
    placeholderData: keepPreviousData,
  });
  const deploys = q.data?.deploys;
  const marks = useMemo(() => deployMarks(deploys ?? []), [deploys]);
  return { marks, error: q.error };
}

/** The charts still say something without their markers, so they stay, and this says what is missing. */
export function DeployNote({ error }: { error: Error | null }) {
  if (!error) return null;
  return <p className="mt-1 text-xs text-kumo-danger">Deploy markers could not be read: {error.message}</p>;
}
