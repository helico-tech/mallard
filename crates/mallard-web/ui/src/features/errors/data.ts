import { useKumoToastManager } from "@cloudflare/kumo";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError } from "@/api/client";
import { sentry } from "@/api/sentry";
import type { IssueDetail, IssueList, IssueStatus } from "@/api/types";

export function useProjects() {
  return useQuery({
    queryKey: ["sentry", "projects"],
    queryFn: ({ signal }) => sentry.projects(signal),
    refetchInterval: (q) => (q.state.data ? false : 5000),
  });
}

/** R / I / U: optimistic everywhere, rolled back on failure, then reloaded. */
export function useSetStatus(project: string) {
  const client = useQueryClient();
  const toasts = useKumoToastManager();
  return useMutation({
    mutationFn: ({ id, status }: { id: string; title: string; status: IssueStatus }) => sentry.setStatus(project, id, status),
    onMutate: async ({ id, status }) => {
      await client.cancelQueries({ queryKey: ["sentry", "issues", project] });
      await client.cancelQueries({ queryKey: ["sentry", "issue", project, id] });
      const lists = client.getQueriesData({ queryKey: ["sentry", "issues", project] });
      const details = client.getQueriesData({ queryKey: ["sentry", "issue", project, id] });
      client.setQueriesData<{ pages: IssueList[]; pageParams: unknown[] }>({ queryKey: ["sentry", "issues", project] }, (old) =>
        old && { ...old, pages: old.pages.map((p) => ({ ...p, issues: p.issues.map((i) => (i.id === id ? { ...i, status } : i)) })) },
      );
      client.setQueriesData<IssueDetail>({ queryKey: ["sentry", "issue", project, id] }, (old) => old && { ...old, status });
      return { lists, details };
    },
    onError: (e, { id }, ctx) => {
      ctx?.lists.forEach(([k, v]) => client.setQueryData(k, v));
      ctx?.details.forEach(([k, v]) => client.setQueryData(k, v));
      toasts.add({ title: `Could not update ${id}`, description: e instanceof ApiError ? e.message : String(e), variant: "error" });
    },
    onSuccess: (_d, { status, title }) => {
      toasts.add({ title: `${status[0]!.toUpperCase()}${status.slice(1)}`, description: title.slice(0, 60), variant: "success" });
    },
    onSettled: () => {
      void client.invalidateQueries({ queryKey: ["sentry", "issues", project] });
      void client.invalidateQueries({ queryKey: ["sentry", "issue", project] });
    },
  });
}
