import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import { LinkProvider, Toasty, TooltipProvider } from "@cloudflare/kumo";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { forwardRef, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter, Link } from "react-router-dom";
import { ApiError } from "@/api/client";
import { SettingsProvider } from "@/app/settings";
import { App } from "./App";
import "./index.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // A failed refresh keeps the data; the screens say it is stale.
      retry: (count, error) => {
        if (error instanceof ApiError && !["unavailable", "internal"].includes(error.kind)) return false;
        return count < 1;
      },
      refetchOnWindowFocus: false,
      staleTime: 2000,
    },
  },
});

/** Kumo's links and side nav go through the router, without a page load. */
const RouterLink = forwardRef<HTMLAnchorElement, React.AnchorHTMLAttributes<HTMLAnchorElement>>(function RouterLink({ href = "", ...rest }, ref) {
  if (/^https?:/.test(href)) return <a ref={ref} href={href} {...rest} />;
  return <Link ref={ref} to={href} {...rest} />;
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <LinkProvider component={RouterLink}>
          <TooltipProvider>
            <SettingsProvider>
              <Toasty>
                <App />
              </Toasty>
            </SettingsProvider>
          </TooltipProvider>
        </LinkProvider>
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
);
