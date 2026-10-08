import { useCallback, useEffect, useRef, useState } from "react";
import { invokeBackend } from "../lib/platform";

export interface ProviderUsageWindow {
  id: string;
  label: string;
  used_percent: number | null;
  resets_at: number | null;
  used: number | null;
  limit: number | null;
  unit: string | null;
  details: { label: string; used: number }[];
}

export interface ProviderUsage {
  id: string;
  name: string;
  status: "ready" | "error" | "not_connected";
  plan: string | null;
  windows: ProviderUsageWindow[];
  note: string | null;
  error: string | null;
  updated_at: number | null;
}

export function useProviderUsage() {
  const [providers, setProviders] = useState<ProviderUsage[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const pending = useRef<Promise<void> | null>(null);
  const mounted = useRef(true);

  const refresh = useCallback((): Promise<void> => {
    if (pending.current) return pending.current;
    setLoading(true);
    pending.current = invokeBackend<ProviderUsage[]>("get_provider_usage")
      .then((data) => {
        if (!mounted.current) return;
        setProviders(data);
        setError(null);
      })
      .catch((cause: unknown) => {
        if (mounted.current) setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        pending.current = null;
        if (mounted.current) setLoading(false);
      });
    return pending.current;
  }, []);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5 * 60 * 1000);
    return () => {
      mounted.current = false;
      window.clearInterval(timer);
    };
  }, [refresh]);

  return { providers, loading, error, refresh };
}
