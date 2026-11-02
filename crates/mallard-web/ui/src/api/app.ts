// What only the whole server knows: the list of apps, and where an id points.
import { get } from "./client";
import type { AppList, FindResult } from "./types";

type Sig = AbortSignal | undefined;

export const apps = {
  list: (since: string, signal?: Sig) => get<AppList>("/api/v1/apps", { since }, signal),
  find: (id: string, since: string, signal?: Sig) => get<FindResult>("/api/v1/find", { id, since }, signal),
};
