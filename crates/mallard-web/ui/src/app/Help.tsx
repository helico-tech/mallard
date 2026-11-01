import { Button, Dialog, Switch } from "@cloudflare/kumo";
import { XIcon } from "@phosphor-icons/react";
import { KeyHint } from "@/components/ui-bits";
import { SCREEN_KEYS, SHARED_KEYS } from "./keys";
import { useSettings } from "./settings";

function Keys({ rows }: { rows: [string, string][] }) {
  return (
    <dl className="grid grid-cols-[136px_minmax(0,1fr)] items-baseline gap-x-4 gap-y-1">
      {rows.map(([k, what]) => (
        <div key={k} className="contents">
          <dt className="flex flex-wrap gap-1">{k.split(/\s{2,}/).map((part) => <KeyHint key={part}>{part}</KeyHint>)}</dt>
          <dd className="text-kumo-default">{what}</dd>
        </div>
      ))}
    </dl>
  );
}

export function HelpDialog() {
  const { helpOpen, setHelpOpen, shortcuts, setShortcuts } = useSettings();
  return (
    <Dialog.Root open={helpOpen} onOpenChange={setHelpOpen}>
      <Dialog size="xl" className="max-h-[85dvh] overflow-y-auto px-6 py-5">
        <div className="flex items-start justify-between gap-4">
          <Dialog.Title>Keys</Dialog.Title>
          <Dialog.Close render={(p) => <Button {...p} variant="ghost" shape="square" aria-label="Close" icon={<XIcon />} />} />
        </div>
        <Dialog.Description className="mt-1.5 text-kumo-subtle">
          The same keys as <code className="mono">mallard tui</code>. All times are UTC. Data refreshes every 5s.
        </Dialog.Description>
        <div className="mt-4 grid gap-6 sm:grid-cols-2">
          <div>
            <h3 className="mb-2 font-medium">Everywhere</h3>
            <Keys rows={SHARED_KEYS} />
          </div>
          <div className="space-y-4">
            {SCREEN_KEYS.map((s) => (
              <div key={s.title}>
                <h3 className="mb-2 font-medium">{s.title}</h3>
                <Keys rows={s.keys} />
              </div>
            ))}
          </div>
        </div>
        <div className="mt-4">
          <h3 className="mb-2 font-medium">Filters</h3>
          <dl className="grid grid-cols-[136px_minmax(0,1fr)] items-baseline gap-x-4 gap-y-1">
            <dt>Traces</dt>
            <dd className="mono [overflow-wrap:anywhere]">service= op= min=200ms max=2s errors &gt;200 key=value</dd>
            <dt>Logs</dt>
            <dd className="mono [overflow-wrap:anywhere]">service= level=warn trace= key=value, other words search the text</dd>
          </dl>
        </div>
        <p className="mt-4 text-xs text-kumo-subtle">Writes are capitals, so r can never change data. Tab keeps its browser meaning: it moves the focus.</p>
        <div className="mt-4 border-t border-kumo-hairline pt-3">
          <Switch label="Single-key shortcuts" checked={shortcuts} onCheckedChange={setShortcuts} />
        </div>
      </Dialog>
    </Dialog.Root>
  );
}
