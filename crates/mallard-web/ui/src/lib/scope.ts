// How the app scope and a `service=` filter combine. An app can have several service
// names (ADR 0028), and a filter can only narrow it.

/** The `service` parameters to send: `{}` is every service. null: this filter names a
 *  service outside the app, so nothing can match and no read is needed. */
export function serviceParam(filter: string | undefined, scope: string[] | null): { service?: string[] } | null {
  if (!scope) return filter ? { service: [filter] } : {};
  if (!filter) return { service: scope };
  return scope.includes(filter) ? { service: [filter] } : null;
}
