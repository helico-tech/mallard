import { useState, type FormEvent } from "react";
import { Banner, Button, Input, LayerCard, Text } from "@cloudflare/kumo";
import { WarningCircleIcon } from "@phosphor-icons/react";
import { useQueryClient } from "@tanstack/react-query";
import { ApiError, get, send, type Session } from "@/api/client";
import { Logo } from "./Logo";

export function Login() {
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const client = useQueryClient();

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await send("POST", "/web/login", { token });
      const session = await get<Session>("/web/session");
      if (!session.authenticated) {
        setError("The browser refused the sign-in cookie: this address needs HTTPS. Put TLS in front of mallard web.");
        return;
      }
      setToken("");
      await client.invalidateQueries();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : "Could not sign in");
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="grid min-h-dvh place-items-center bg-kumo-canvas p-4">
      <LayerCard className="w-full max-w-sm">
        <LayerCard.Primary className="px-5 py-4">
        <form onSubmit={(e) => void submit(e)} className="flex flex-col gap-4">
        <div className="flex items-center gap-2.5">
          <Logo className="size-8" />
          <Text as="h1" variant="heading">Sign in to mallard</Text>
        </div>
        <Text variant="secondary">Use an API token: one of the server's <code className="mono">server.api_tokens</code>.</Text>
        <Input type="password" label="API token" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" autoFocus />
        {error && <Banner size="sm" variant="error" icon={<WarningCircleIcon weight="fill" />} title={error} />}
        <Button type="submit" variant="primary" className="w-full" disabled={!token.trim() || busy} loading={busy}>Sign in</Button>
      </form>
        </LayerCard.Primary>
      </LayerCard>
    </main>
  );
}
