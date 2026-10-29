import { useEffect, useState } from "react";
import type { ProviderUsage, ProviderUsageWindow } from "../hooks/useProviderUsage";

function resetLabel(timestamp: number | null, now: number) {
  if (timestamp === null) return "Reset time unavailable";
  const minutes = Math.ceil((timestamp * 1000 - now) / 60000);
  if (minutes <= 0) return "Reset due · refresh to confirm";
  if (minutes < 60) return `Resets in ${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `Resets in ${hours}h ${minutes % 60}m`;
  return `Resets in ${Math.floor(hours / 24)}d ${hours % 24}h`;
}

function UsageMeter({ window: quota, now }: { window: ProviderUsageWindow; now: number }) {
  const remaining = quota.used_percent === null ? null : Math.max(0, Math.min(100, 100 - quota.used_percent));
  const color = remaining === null ? "bg-gray-300" : remaining <= 10 ? "bg-red-500" : remaining <= 25 ? "bg-amber-500" : "bg-emerald-500";
  const formatAmount = (value: number) => quota.unit && /^[A-Z]{3}$/.test(quota.unit)
    ? new Intl.NumberFormat(undefined, { style: "currency", currency: quota.unit, maximumFractionDigits: 2 }).format(value)
    : new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(value);
  return (
    <div className="space-y-1.5">
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1 text-sm">
        <span className="font-medium">{quota.label}</span>
        <span className="tabular-nums text-gray-600 dark:text-gray-300" title={quota.used_percent === null ? undefined : `${quota.used_percent}% used`}>
          {remaining === null ? "Usage unavailable" : `${Math.round(remaining * 10) / 10}% remaining`}
        </span>
      </div>
      {remaining !== null && (
        <div role="meter" aria-label={`${quota.label} remaining`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={remaining}
          className="h-2 overflow-hidden rounded-full bg-gray-100 dark:bg-gray-800">
          <div className={`h-full rounded-full transition-all ${color}`} style={{ width: `${remaining}%` }} />
        </div>
      )}
      <div className="flex flex-wrap justify-between gap-1 text-xs text-gray-500 dark:text-gray-400">
        <span title={quota.resets_at === null ? undefined : new Date(quota.resets_at * 1000).toLocaleString()}>{resetLabel(quota.resets_at, now)}</span>
        {quota.used !== null && (
          <span>{formatAmount(quota.used)}{quota.limit !== null ? ` / ${formatAmount(quota.limit)}` : ""} {quota.unit === "calls" ? "calls" : "used"}</span>
        )}
      </div>
      {quota.details.length > 0 && (
        <details className="text-xs text-gray-500 dark:text-gray-400">
          <summary className="cursor-pointer">Tool usage breakdown</summary>
          <dl className="mt-2 space-y-1">
            {quota.details.map((detail, index) => <div key={`${detail.label}-${index}`} className="flex justify-between gap-2"><dt>{detail.label}</dt><dd>{detail.used.toLocaleString()}</dd></div>)}
          </dl>
        </details>
      )}
    </div>
  );
}

export function ProviderUsagePanel({ providers, loading, error, onRefresh, compact = false }: {
  providers: ProviderUsage[];
  loading: boolean;
  error: string | null;
  onRefresh: (force?: boolean) => Promise<void>;
  compact?: boolean;
}) {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30000);
    return () => window.clearInterval(timer);
  }, []);

  return (
    <section aria-labelledby="provider-usage-heading" className={compact ? "mb-3" : "mb-8"}>
      {!compact && <div className="mb-4 flex items-center justify-between gap-3">
        <div>
          <h2 id="provider-usage-heading" className="text-base font-semibold">AI usage limits</h2>
          <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">Your connected providers · checks every 5 minutes</p>
        </div>
        <button onClick={() => void onRefresh(true)} disabled={loading} className="rounded-lg border border-gray-200 px-3 py-2 text-xs font-medium hover:bg-gray-100 disabled:opacity-50 dark:border-gray-700 dark:hover:bg-gray-800">
          {loading ? "Checking…" : "Refresh limits"}
        </button>
      </div>}
      {compact && <h2 id="provider-usage-heading" className="px-2 pb-1 text-[11px] font-semibold uppercase tracking-wide text-gray-500 dark:text-gray-400">AI usage limits</h2>}
      {error && <p role="alert" className="mb-3 rounded-lg bg-amber-50 p-3 text-sm text-amber-800 dark:bg-amber-950/40 dark:text-amber-200">Could not refresh provider limits. {providers.length > 0 && "Showing the last received data. "}{error}</p>}
      {loading && providers.length === 0 && <p role="status" className="py-6 text-sm text-gray-500">Reading Claude Code and Zcode usage…</p>}
      <div className={compact ? "space-y-2" : "grid items-start gap-4 md:grid-cols-2"}>
        {providers.map((provider) => {
          const stale = Boolean(error) || provider.status === "error" || (provider.updated_at !== null && now - provider.updated_at * 1000 > 10 * 60 * 1000);
          return (
            <article key={provider.id} aria-label={`${provider.name} usage`} className={`border border-gray-200 bg-white dark:border-gray-800 dark:bg-gray-900 ${compact ? "rounded-lg p-3" : "rounded-2xl p-5"}`}>
              <div className={`flex flex-wrap items-center justify-between gap-2 ${compact ? "mb-3 text-sm" : "mb-5"}`}>
                <div className="flex flex-wrap items-center gap-2">
                  <h3 className="font-semibold">{provider.name}</h3>
                  {provider.plan && <span className="rounded-md bg-gray-100 px-2 py-0.5 text-xs capitalize text-gray-600 dark:bg-gray-800 dark:text-gray-300">{provider.plan}</span>}
                </div>
                <span className={`text-xs ${stale ? "text-amber-600 dark:text-amber-400" : "text-gray-500 dark:text-gray-400"}`}>
                  {provider.status === "not_connected" ? "Not connected" : stale ? provider.updated_at ? "Last known usage" : "Unavailable" : "Connected"}
                </span>
              </div>
              {provider.error && <p className="mb-4 text-sm text-amber-700 dark:text-amber-300">{provider.error}</p>}
              <div className={compact ? "space-y-3" : "space-y-5"}>{(compact ? provider.windows.slice(0, 2) : provider.windows).map((quota) => <UsageMeter key={quota.id} window={quota} now={now} />)}</div>
              {compact && provider.windows.length > 2 && <details className="mt-3">
                <summary className="cursor-pointer text-xs text-gray-500 dark:text-gray-400">{provider.windows.length - 2} more {provider.windows.length === 3 ? "limit" : "limits"}</summary>
                <div className="mt-3 space-y-3">{provider.windows.slice(2).map((quota) => <UsageMeter key={quota.id} window={quota} now={now} />)}</div>
              </details>}
              {provider.note && !compact && <p className="mt-4 text-xs text-gray-500 dark:text-gray-400">{provider.note}</p>}
              {provider.updated_at !== null && <p className={`text-xs text-gray-400 dark:text-gray-500 ${compact ? "mt-2" : "mt-5 border-t border-gray-100 pt-3 dark:border-gray-800"}`}>Updated {new Date(provider.updated_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}{stale && " · may be out of date"}</p>}
            </article>
          );
        })}
      </div>
    </section>
  );
}
