import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import "./i18n";
import App from "./App";
import { AuthProvider } from "./auth";
import { ErrorBoundary } from "./components/ErrorBoundary";
// Bundle the three UI / reader fonts so every browser renders with the same
// letterforms. Variable-weight woff2 — one file per family covers every weight
// the styles reference (450 / 500 / 550 / 600 / 650 / 700).
import "@fontsource-variable/inter-tight";
import "@fontsource-variable/jetbrains-mono";
import "@fontsource-variable/newsreader";
import "@fontsource-variable/newsreader/wght-italic.css";
import "./styles.css";

// Web SPA — no native titlebar / drag region.
document.documentElement.dataset.platform = "web";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 30_000, refetchOnWindowFocus: false, retry: 1 },
  },
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <AuthProvider>
          <App />
        </AuthProvider>
      </QueryClientProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
