# UI/UX guidelines for the Web UI

How we build data-heavy screens. **[S]** = checked at the source (via a fetch
summary, not word for word). **[J]** = our judgement. Where the TUI already
decided something (ADR 0006, 0008), the TUI wins; differences are listed at the end.

## Density and layout
- [S] First column is a readable identifier, not a raw id; related columns together.
- [S] Freeze the header. Keep your place with row hover and borders.
- [S] Details open in a non-modal side panel, so the list stays visible; no modals over rows.
- [S] Data text 14px, headings 16px and up, inline monospace about 0.9em (Kumo).
- [S] Numbers use `tabular-nums`.
- [J] Fixed left nav; one top bar with the app, the time window and live state; every detail has its own URL.
- [J] Monospace only for ids, hashes, log bodies, code and frames; never for labels.
- [J] Fixed widths for time, level and numbers; one flexible column. Truncate at the
  end, ids and paths in the middle or on the left.
- [J] A truncated value is always reachable: tooltip, copy, or the detail pane.

## Tables
- [S] Row heights 24, 32, 40, 48, 64; header matches (Carbon). [J] 32 by default, 24 for logs.
- [S] Only the sorted column shows an arrow.
- [S] One or two inline row actions at most; never hover-only.
- [S] No infinite scroll when people look for one item: Load More, a visible end, and
  position restored on return.
- [J] Virtualise only past a few hundred rows; [S] then set `aria-rowcount` and `aria-rowindex`.
- [J] Key rows by id. A refresh keeps selection, focus and scroll.
- [J] Numbers right-aligned, unit in the header; times left-aligned.

## Filters and time
- [S] The URL holds window, filters, grouping and tab; never hover, loading or secrets.
- [S] `pushState` for real changes, `replaceState` for small tweaks.
- [S] Active filters are obvious; "Clear" shows only when something is active.
- [S] Time window, filtering and chart inspection behave the same on every page.
- [J] The window control is on every data page, same place. State "UTC" once, there.

## Live data
- [S] Auto-update needs pause or stop (WCAG 2.2.2).
- [S] Keep previous data during a refetch; disable Next until real data arrives.
- [S] Under 1s no indicator; full page under 10s a skeleton; one panel a small spinner.
- [S] Announce status changes without moving focus (WCAG 4.1.3).
- [J] A refresh never shifts layout. Show "updated 40s ago"; after a failed refresh keep the data and mark it stale.
- [J] Optimistic updates only for simple writes (resolve, ignore), with rollback; never for query results.

## Charts
- [S] Amounts as length or position; colour for categories. No pie, donut, gauge, 3D.
- [S] Status is colour plus icon, shape or text (WCAG 1.4.1).
- [S] Tooltips can be dismissed and hovered, and stay (WCAG 1.4.13).
- [J] Sparklines in rows show trend only: no axes, same window per column, value beside them.
- [J] One axis per chart. Never draw missing data as zero.

## Traces, stacks, logs
- [S] Waterfall: tree left, bars right, detail in a side panel, selected span in the URL,
  subtrees collapse with a count, colour by service, errors stand out; mark orphans.
- [S] Logs: colour by level; open a line for details; line-wrap toggle; pause and resume; volume histogram.
- [S] `role="log"` is a polite live region and needs a name.
- [J] Severity gutter is a colour bar plus level text. Follow stops when you scroll up.
- [J] Stacks: app frames stand out; file, line and function in monospace; copy for raw.

## Keyboard
- [S] `?` opens the shortcut list.
- [S] Single-key shortcuts can be switched off (WCAG 2.1.4, Level A).
- [S] Plain `<table>` for read-only data; `role="grid"` makes you code everything.
- [S] Focus is not hidden by sticky bars (2.4.11); targets at least 24×24px (2.5.8).
- [J] `j`/`k` move the row, Enter opens, Esc returns focus to the row. Shortcuts are ignored while typing.

## States
- [S] Never say "no records" while loading. Empty states say why and link the next action.
- [J] Three empties: no data ever (how to set up), nothing matches (the filters, Clear), nothing in this window (widen it).
- [J] Errors say what failed, offer Retry and a hint, and keep the last good data.
- [J] Panels fail alone. Say when a result is incomplete.

## Theme and motion
- [S] Text 4.5:1, large text and UI parts 3:1. Semantic tokens per theme, no hard-coded colours.
- [S] Respect `prefers-reduced-motion`. [J] No pulsing live dots or animated chart refreshes.

## Anti-patterns
A refresh that resets scroll or selection. A time window lost on navigation. Filters
not in the URL. Red and green as the only signal. Spinners replacing data on screen.
Missing data drawn as zero. Hover-only actions. Silent truncation. Dead ends: a trace
with no link to its logs and errors.

## Where the TUI decides otherwise
- Refresh every 5s (sources advise a minute or more): the TUI's cadence, with a pause button.
- One app and one time window for the whole UI, in the address (ADR 0028). A preset a side refuses is not offered.

## Sources
[NN/g data tables](https://www.nngroup.com/articles/data-tables/),
[NN/g infinite scrolling](https://www.nngroup.com/articles/infinite-scrolling-tips/),
[NN/g skeleton screens](https://www.nngroup.com/articles/skeleton-screens/),
[NN/g empty states](https://www.nngroup.com/articles/empty-state-interface-design/),
[NN/g dashboards](https://www.nngroup.com/articles/dashboards-preattentive/),
[Carbon data table](https://carbondesignsystem.com/components/data-table/usage/),
[Carbon status](https://carbondesignsystem.com/patterns/status-indicator-pattern/),
[Kumo](https://kumo-ui.com/skill.md),
[Dynatrace URL sharing](https://developer.dynatrace.com/develop/navigation/url-sharing/),
[Stripe filter controls](https://docs.stripe.com/stripe-apps/patterns/filter-controls),
[DRUIDS](https://www.datadoghq.com/blog/engineering/druids-the-design-system-that-powers-datadog/),
[WCAG 2.2](https://www.w3.org/TR/WCAG22/),
[ARIA grid](https://www.w3.org/WAI/ARIA/apg/patterns/grid/),
[TanStack paginated queries](https://tanstack.com/query/latest/docs/framework/react/guides/paginated-queries),
[Grafana](https://grafana.com/docs/grafana/latest/visualizations/explore/logs-integration/),
[Honeycomb traces](https://docs.honeycomb.io/investigate/analyze/explore-traces/),
[Sentry trace view](https://docs.sentry.io/concepts/key-terms/tracing/trace-view/),
[Linear shortcuts](https://linear.app/changelog/2021-03-25-keyboard-shortcuts-help),
[Atlassian colour](https://atlassian.design/foundations/color),
[MDN role=log](https://developer.mozilla.org/en-US/docs/Web/Accessibility/ARIA/Reference/Roles/log_role).
