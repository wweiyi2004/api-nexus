import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Activity,
  ArrowDown,
  ArrowUp,
  BarChart3,
  Clock3,
  Download,
  Gauge,
  KeyRound,
  RefreshCw,
  ScrollText,
  Search,
  Trash2,
} from "lucide-react";

export interface RequestLogEntry {
  timestamp: number;
  method: string;
  path: string;
  model: string;
  provider: string;
  api_key_name: string;
  status: number;
  input_tokens: number;
  output_tokens: number;
  cached_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  duration_ms: number;
  error: string | null;
}

interface Provider {
  id: string;
  name: string;
  protocol: "openai" | "anthropic";
  base_url: string;
  api_key: string;
  models: string[];
  enabled: boolean;
  priority: number;
}

export interface ModelPrice {
  model: string;
  input_usd_per_million: number;
  output_usd_per_million: number;
  cached_usd_per_million: number;
  cache_read_usd_per_million: number;
  cache_write_usd_per_million: number;
}

interface AppConfig {
  providers: Provider[];
  model_prices: ModelPrice[];
  usd_to_cny_rate: number;
}

type TrendMetric = "requests" | "tokens" | "cost" | "errors";

interface TrendBucket {
  label: string;
  rangeLabel: string;
  total: number;
  values: Map<string, number>;
}

interface TrendRanking {
  name: string;
  value: number;
  requests: number;
  errors: number;
  lastTs: number;
}

interface TrendSeries {
  name: string;
  color: string;
  value: number;
}

const trendMetricLabels: Record<TrendMetric, string> = {
  requests: "请求数",
  tokens: "Token",
  cost: "费用",
  errors: "错误",
};

const trendColors = ["#0891b2", "#10b981", "#6366f1", "#f59e0b", "#ef4444", "#64748b"];
const unverifiedKeyFilter = "__api_nexus_unverified__";

function formatTime(ts: number) {
  const d = new Date(ts * 1000);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function formatTokens(value: number) {
  return new Intl.NumberFormat("en-US").format(value);
}

function formatMoney(value: number, symbol: string) {
  if (value === 0) return `${symbol}0.0000`;
  return `${symbol}${value < 0.01 ? value.toFixed(6) : value.toFixed(4)}`;
}

function formatTrendValue(value: number, metric: TrendMetric, compact = false) {
  if (metric === "cost") return formatMoney(value, "$");
  const formatter = new Intl.NumberFormat("en-US", {
    notation: compact ? "compact" : "standard",
    maximumFractionDigits: metric === "tokens" && compact ? 1 : 0,
  });
  return formatter.format(Math.round(value));
}

function formatBucketLabel(ts: number, spanSeconds: number) {
  const d = new Date(ts * 1000);
  const pad = (n: number) => n.toString().padStart(2, "0");
  if (spanSeconds <= 24 * 3600) {
    return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }
  if (spanSeconds <= 7 * 86400) {
    return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:00`;
  }
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

function formatRelativeTime(ts: number) {
  const diff = Math.max(0, Math.floor(Date.now() / 1000 - ts));
  if (diff < 60) return `${Math.max(1, diff)} 秒前`;
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  return `${Math.floor(diff / 86400)} 天前`;
}

function statusClass(status: number) {
  if (status >= 200 && status < 300) return "badge-success";
  if (status >= 400 && status < 500) return "badge-warning";
  return "badge-error";
}

function logKeyName(log: RequestLogEntry) {
  return log.api_key_name || "未验证";
}

function trendValueForLog(
  log: RequestLogEntry,
  metric: TrendMetric,
  costFor: (log: RequestLogEntry) => { usd: number; cny: number } | null,
  tokenCountFor: (log: RequestLogEntry) => number,
) {
  if (metric === "requests") return 1;
  if (metric === "tokens") return tokenCountFor(log);
  if (metric === "cost") return costFor(log)?.usd ?? 0;
  return log.status >= 400 || log.error ? 1 : 0;
}

export function buildTrendData(
  logs: RequestLogEntry[],
  timeFilter: string,
  metric: TrendMetric,
  costFor: (log: RequestLogEntry) => { usd: number; cny: number } | null,
  tokenCountFor: (log: RequestLogEntry) => number,
) {
  const sorted = [...logs].sort((a, b) => a.timestamp - b.timestamp);
  const keyStats = new Map<string, TrendRanking>();

  for (const log of sorted) {
    const name = logKeyName(log);
    const current = keyStats.get(name) ?? {
      name,
      value: 0,
      requests: 0,
      errors: 0,
      lastTs: 0,
    };
    current.value += trendValueForLog(log, metric, costFor, tokenCountFor);
    current.requests += 1;
    if (log.status >= 400 || log.error) current.errors += 1;
    current.lastTs = Math.max(current.lastTs, log.timestamp);
    keyStats.set(name, current);
  }

  const rankings = [...keyStats.values()].sort(
    (a, b) => b.value - a.value || b.requests - a.requests || b.lastTs - a.lastTs,
  );

  if (sorted.length === 0) {
    return {
      activeKeys: 0,
      buckets: [] as TrendBucket[],
      rankings,
      series: [] as TrendSeries[],
      maxBucketValue: 1,
      peakBucket: null as TrendBucket | null,
      recentBucket: null as TrendBucket | null,
    };
  }

  let end = Math.floor(Date.now() / 1000);
  let start = sorted[0].timestamp;
  if (timeFilter === "1h") start = end - 3600;
  if (timeFilter === "24h") start = end - 86400;
  if (timeFilter === "7d") start = end - 7 * 86400;
  if (timeFilter === "all") {
    end = Math.max(end, sorted[sorted.length - 1].timestamp);
    if (end - start < 3600) start = end - 3600;
  }

  const span = Math.max(60, end - start);
  const bucketCount =
    timeFilter === "1h"
      ? 12
      : timeFilter === "24h"
        ? 24
        : timeFilter === "7d"
          ? 14
          : Math.min(18, Math.max(8, Math.ceil(span / 3600)));
  const bucketSize = Math.max(60, Math.ceil(span / bucketCount));
  const bucketStart = end - bucketSize * bucketCount;
  const buckets: TrendBucket[] = Array.from({ length: bucketCount }, (_, index) => {
    const bucketTs = bucketStart + index * bucketSize;
    return {
      label: formatBucketLabel(bucketTs, span),
      rangeLabel: `${formatBucketLabel(bucketTs, span)} - ${formatBucketLabel(bucketTs + bucketSize, span)}`,
      total: 0,
      values: new Map<string, number>(),
    };
  });

  const topNames = rankings.slice(0, 5).map((item) => item.name);
  const hasOther = rankings.length > topNames.length;
  const topNameSet = new Set(topNames);

  for (const log of sorted) {
    if (log.timestamp < bucketStart || log.timestamp > end) continue;
    const value = trendValueForLog(log, metric, costFor, tokenCountFor);
    const key = logKeyName(log);
    const seriesName = topNameSet.has(key) ? key : hasOther ? "其他" : key;
    const index = Math.min(
      buckets.length - 1,
      Math.max(0, Math.floor((log.timestamp - bucketStart) / bucketSize)),
    );
    const bucket = buckets[index];
    bucket.total += value;
    bucket.values.set(seriesName, (bucket.values.get(seriesName) ?? 0) + value);
  }

  const seriesNames = hasOther ? [...topNames, "其他"] : topNames;
  const series = seriesNames.map((name, index) => ({
    name,
    color: trendColors[index % trendColors.length],
    value:
      name === "其他"
        ? rankings.slice(5).reduce((sum, item) => sum + item.value, 0)
        : keyStats.get(name)?.value ?? 0,
  }));
  const maxBucketValue = Math.max(...buckets.map((bucket) => bucket.total), 1);
  const peakBucket = buckets.reduce<TrendBucket | null>(
    (peak, bucket) => (!peak || bucket.total > peak.total ? bucket : peak),
    null,
  );

  return {
    activeKeys: keyStats.size,
    buckets,
    rankings,
    series,
    maxBucketValue,
    peakBucket: peakBucket && peakBucket.total > 0 ? peakBucket : null,
    recentBucket: buckets[buckets.length - 1] ?? null,
  };
}

export function calculateLogCost(
  log: RequestLogEntry,
  price: ModelPrice,
  protocol: Provider["protocol"] | undefined,
  usdToCnyRate = 7.2,
) {
  const cacheReadTokens = log.cache_read_tokens ?? log.cached_tokens ?? 0;
  const cacheWriteTokens = log.cache_write_tokens ?? 0;
  const regularInputTokens = protocol === "openai"
    ? Math.max(0, log.input_tokens - cacheReadTokens)
    : log.input_tokens;
  const usd =
    (regularInputTokens / 1_000_000) * price.input_usd_per_million +
    (log.output_tokens / 1_000_000) * price.output_usd_per_million +
    (cacheReadTokens / 1_000_000) * (price.cache_read_usd_per_million ?? price.cached_usd_per_million) +
    (cacheWriteTokens / 1_000_000) * (price.cache_write_usd_per_million ?? 0);
  return { usd, cny: usd * usdToCnyRate };
}

export function countLogTokens(
  log: RequestLogEntry,
  protocol: Provider["protocol"] | undefined,
) {
  const separateCachedTokens = protocol === "openai"
    ? 0
    : (log.cache_read_tokens ?? 0) + (log.cache_write_tokens ?? 0);
  return log.input_tokens + log.output_tokens + separateCachedTokens;
}

export default function Logs() {
  const [logs, setLogs] = useState<RequestLogEntry[]>([]);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [modelFilter, setModelFilter] = useState("all");
  const [providerFilter, setProviderFilter] = useState("all");
  const [keyFilter, setKeyFilter] = useState("all");
  const [timeFilter, setTimeFilter] = useState("all");
  const [trendMetric, setTrendMetric] = useState<TrendMetric>("requests");
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(1);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const pageSize = 20;

  const fetchLogs = async () => {
    try {
      const [entries, appConfig] = await Promise.all([
        invoke<RequestLogEntry[]>("get_request_logs"),
        invoke<AppConfig>("get_config"),
      ]);
      setLogs(entries);
      setConfig(appConfig);
    } catch (e) {
      console.error(e);
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchLogs();
    if (!autoRefresh) return;
    const interval = setInterval(fetchLogs, 2000);
    return () => clearInterval(interval);
  }, [autoRefresh]);

  const clearLogs = async () => {
    try {
      setError(null);
      await invoke("clear_request_logs");
      setLogs([]);
    } catch (e) {
      console.error(e);
      setError(String(e));
    }
  };

  const exportLogs = async () => {
    try {
      setError(null);
      const path = await invoke<string>("export_request_logs_csv");
      setNotice(`已导出到 ${path}`);
      setTimeout(() => setNotice(null), 5000);
    } catch (e) {
      console.error(e);
      setError(String(e));
    }
  };

  const modelOptions = Array.from(new Set(logs.map((log) => log.model).filter(Boolean))).sort();
  const providerOptions = Array.from(new Set(logs.map((log) => log.provider).filter(Boolean))).sort();
  const keyOptions = Array.from(new Set(logs.map((log) => log.api_key_name).filter(Boolean))).sort();

  const priceMap = new Map(
    (config?.model_prices ?? [])
      .filter((price) => price.model.trim())
      .map((price) => [price.model.trim().toLowerCase(), price]),
  );
  const providerProtocolMap = new Map<string, Provider["protocol"]>();
  for (const provider of config?.providers ?? []) {
    providerProtocolMap.set(provider.id, provider.protocol);
    providerProtocolMap.set(provider.name, provider.protocol);
  }

  const filteredLogs = logs.filter((log) => {
    if (modelFilter !== "all" && log.model !== modelFilter) return false;
    if (providerFilter !== "all" && log.provider !== providerFilter) return false;
    if (keyFilter === unverifiedKeyFilter && log.api_key_name) return false;
    if (
      keyFilter !== "all" &&
      keyFilter !== unverifiedKeyFilter &&
      log.api_key_name !== keyFilter
    ) return false;
    if (query.trim()) {
      const q = query.toLowerCase();
      const text = `${log.path} ${log.model} ${log.provider} ${log.api_key_name} ${log.error ?? ""}`.toLowerCase();
      if (!text.includes(q)) return false;
    }
    if (timeFilter !== "all") {
      const seconds = timeFilter === "1h" ? 3600 : timeFilter === "24h" ? 86400 : 7 * 86400;
      if (Date.now() / 1000 - log.timestamp > seconds) return false;
    }
    return true;
  });

  const costFor = (log: RequestLogEntry) => {
    const price = priceMap.get(log.model.trim().toLowerCase());
    if (!price) return null;
    return calculateLogCost(
      log,
      price,
      providerProtocolMap.get(log.provider),
      config?.usd_to_cny_rate ?? 7.2,
    );
  };

  const tokenCountFor = (log: RequestLogEntry) =>
    countLogTokens(log, providerProtocolMap.get(log.provider));

  const totals = filteredLogs.reduce(
    (acc, log) => {
      const cost = costFor(log);
      acc.input += log.input_tokens;
      acc.output += log.output_tokens;
      acc.cacheRead += log.cache_read_tokens ?? log.cached_tokens ?? 0;
      acc.cacheWrite += log.cache_write_tokens ?? 0;
      acc.usd += cost?.usd ?? 0;
      return acc;
    },
    { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, usd: 0 },
  );
  const trendData = buildTrendData(filteredLogs, timeFilter, trendMetric, costFor, tokenCountFor);
  const rankingMax = Math.max(...trendData.rankings.map((item) => item.value), 1);

  const pageCount = Math.max(1, Math.ceil(filteredLogs.length / pageSize));
  const safePage = Math.min(page, pageCount);
  const pagedLogs = filteredLogs.slice((safePage - 1) * pageSize, safePage * pageSize);

  useEffect(() => {
    setPage(1);
  }, [modelFilter, providerFilter, keyFilter, timeFilter, query]);

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="metric-label">Request Log</div>
          <h1 className="mt-1 text-2xl font-semibold text-surface-950 dark:text-white">
            请求日志
          </h1>
        </div>
        <div className="flex items-center gap-2">
          <button
            className={autoRefresh ? "btn-primary" : "btn-secondary"}
            onClick={() => setAutoRefresh((v) => !v)}
          >
            <Activity className="h-4 w-4" />
            {autoRefresh ? "自动刷新中" : "已暂停"}
          </button>
          <button className="btn-secondary" onClick={fetchLogs}>
            <RefreshCw className="h-4 w-4" />
            刷新
          </button>
          <button className="btn-secondary" onClick={exportLogs} disabled={logs.length === 0}>
            <Download className="h-4 w-4" />
            导出 CSV
          </button>
          <button className="btn-secondary" onClick={clearLogs} disabled={logs.length === 0}>
            <Trash2 className="h-4 w-4" />
            清空
          </button>
        </div>
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          {error}
        </div>
      )}
      {notice && (
        <div className="rounded-lg border border-emerald-200 bg-emerald-50 px-4 py-3 text-sm text-emerald-700 dark:border-emerald-900/60 dark:bg-emerald-950/30 dark:text-emerald-300">
          {notice}
        </div>
      )}

      <section className="panel p-4">
        <div className="grid grid-cols-1 gap-3 lg:grid-cols-[1fr_repeat(4,12rem)]">
          <label className="relative">
            <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-surface-400" />
            <input
              className="input-field pl-9"
              placeholder="搜索路径、模型、服务商、密钥或错误"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <select className="input-field" value={modelFilter} onChange={(e) => setModelFilter(e.target.value)}>
            <option value="all">全部模型</option>
            {modelOptions.map((model) => (
              <option key={model} value={model}>{model}</option>
            ))}
          </select>
          <select className="input-field" value={providerFilter} onChange={(e) => setProviderFilter(e.target.value)}>
            <option value="all">全部服务商</option>
            {providerOptions.map((provider) => (
              <option key={provider} value={provider}>{provider}</option>
            ))}
          </select>
          <select className="input-field" value={keyFilter} onChange={(e) => setKeyFilter(e.target.value)}>
            <option value="all">全部密钥</option>
            {logs.some((log) => !log.api_key_name) && (
              <option value={unverifiedKeyFilter}>未验证</option>
            )}
            {keyOptions.map((keyName) => (
              <option key={keyName} value={keyName}>{keyName}</option>
            ))}
          </select>
          <select className="input-field" value={timeFilter} onChange={(e) => setTimeFilter(e.target.value)}>
            <option value="all">全部时间</option>
            <option value="1h">最近 1 小时</option>
            <option value="24h">最近 24 小时</option>
            <option value="7d">最近 7 天</option>
          </select>
        </div>
        <div className="mt-3 grid grid-cols-2 gap-2 text-sm lg:grid-cols-6">
          <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
            <div className="metric-label">Requests</div>
            <div className="mt-1 font-semibold">{filteredLogs.length}</div>
          </div>
          <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
            <div className="metric-label">Input</div>
            <div className="mt-1 font-semibold text-emerald-600 dark:text-emerald-300">{formatTokens(totals.input)}</div>
          </div>
          <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
            <div className="metric-label">Output</div>
            <div className="mt-1 font-semibold text-sky-600 dark:text-sky-300">{formatTokens(totals.output)}</div>
          </div>
          <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
            <div className="metric-label">Cache Read</div>
            <div className="mt-1 font-semibold text-violet-600 dark:text-violet-300">{formatTokens(totals.cacheRead)}</div>
          </div>
          <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
            <div className="metric-label">Cache Write</div>
            <div className="mt-1 font-semibold text-fuchsia-600 dark:text-fuchsia-300">{formatTokens(totals.cacheWrite)}</div>
          </div>
          <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
            <div className="metric-label">Cost</div>
            <div className="mt-1 font-semibold">
              {formatMoney(totals.usd, "$")} / {formatMoney(totals.usd * (config?.usd_to_cny_rate ?? 7.2), "¥")}
            </div>
          </div>
        </div>
      </section>

      {!loading && logs.length > 0 && (
        <section className="grid grid-cols-1 gap-3 xl:grid-cols-[minmax(0,1.65fr)_minmax(20rem,0.85fr)]">
          <div className="panel p-4">
            <div className="flex flex-wrap items-start justify-between gap-3">
              <div>
                <div className="flex items-center gap-2">
                  <BarChart3 className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
                  <h2 className="text-sm font-semibold">小号趋势</h2>
                </div>
                <p className="mt-1 text-xs text-surface-500 dark:text-surface-400">
                  按 {trendMetricLabels[trendMetric]} 统计，跟随当前筛选条件
                </p>
              </div>
              <div className="grid grid-cols-2 gap-1 rounded-lg border border-surface-200 bg-surface-50 p-1 dark:border-surface-800 dark:bg-surface-950 sm:flex">
                {(["requests", "tokens", "cost", "errors"] as TrendMetric[]).map((metric) => (
                  <button
                    key={metric}
                    className={`rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors ${
                      trendMetric === metric
                        ? "bg-surface-900 text-white dark:bg-cyan-500 dark:text-surface-950"
                        : "text-surface-600 hover:bg-white dark:text-surface-300 dark:hover:bg-surface-900"
                    }`}
                    onClick={() => setTrendMetric(metric)}
                  >
                    {trendMetricLabels[metric]}
                  </button>
                ))}
              </div>
            </div>

            <div className="mt-4 grid grid-cols-3 gap-2">
              <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                <div className="metric-label">Active Keys</div>
                <div className="mt-1 flex items-center gap-2 font-semibold">
                  <KeyRound className="h-3.5 w-3.5 text-amber-600 dark:text-amber-300" />
                  {trendData.activeKeys}
                </div>
              </div>
              <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                <div className="metric-label">Peak</div>
                <div className="mt-1 flex items-center gap-2 font-semibold">
                  <Gauge className="h-3.5 w-3.5 text-cyan-600 dark:text-cyan-300" />
                  {trendData.peakBucket
                    ? formatTrendValue(trendData.peakBucket.total, trendMetric, true)
                    : "0"}
                </div>
              </div>
              <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                <div className="metric-label">Latest</div>
                <div className="mt-1 flex items-center gap-2 font-semibold">
                  <Clock3 className="h-3.5 w-3.5 text-emerald-600 dark:text-emerald-300" />
                  {trendData.recentBucket
                    ? formatTrendValue(trendData.recentBucket.total, trendMetric, true)
                    : "0"}
                </div>
              </div>
            </div>

            {filteredLogs.length === 0 ? (
              <div className="mt-4 flex h-56 items-center justify-center rounded-lg border border-dashed border-surface-300 text-sm text-surface-500 dark:border-surface-700 dark:text-surface-400">
                当前筛选无可视化数据
              </div>
            ) : (
              <>
                <div className="mt-4 overflow-x-auto">
                  <div className="min-w-[42rem]">
                    <div className="flex h-48 items-end gap-1 border-b border-surface-200 pb-2 dark:border-surface-800">
                      {trendData.buckets.map((bucket, index) => (
                        <div key={`${bucket.label}-${index}`} className="group flex h-full min-w-10 flex-1 flex-col justify-end">
                          <div className="relative flex min-h-0 flex-1 items-end justify-center">
                            <div
                              className="w-full max-w-8 overflow-hidden rounded-t-md bg-surface-100 ring-1 ring-surface-200 dark:bg-surface-800 dark:ring-surface-700"
                              style={{
                                height: `${bucket.total > 0 ? Math.max(4, (bucket.total / trendData.maxBucketValue) * 100) : 4}%`,
                                opacity: bucket.total > 0 ? 1 : 0.45,
                              }}
                              title={`${bucket.rangeLabel}: ${formatTrendValue(bucket.total, trendMetric)}`}
                            >
                              {bucket.total > 0 && (
                                <div className="flex h-full w-full flex-col-reverse">
                                  {trendData.series.map((series) => {
                                    const value = bucket.values.get(series.name) ?? 0;
                                    if (value <= 0) return null;
                                    return (
                                      <div
                                        key={series.name}
                                        style={{
                                          height: `${(value / bucket.total) * 100}%`,
                                          backgroundColor: series.color,
                                        }}
                                      />
                                    );
                                  })}
                                </div>
                              )}
                            </div>
                            <div className="pointer-events-none absolute bottom-full left-1/2 z-10 mb-2 hidden min-w-44 -translate-x-1/2 rounded-md border border-surface-200 bg-white px-3 py-2 text-xs shadow-lg group-hover:block dark:border-surface-700 dark:bg-surface-950">
                              <div className="font-medium text-surface-800 dark:text-surface-100">
                                {bucket.rangeLabel}
                              </div>
                              <div className="mt-1 font-mono text-surface-500 dark:text-surface-400">
                                {formatTrendValue(bucket.total, trendMetric)}
                              </div>
                            </div>
                          </div>
                          <div className="mt-2 truncate text-center text-[10px] text-surface-500 dark:text-surface-400">
                            {bucket.label}
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>
                </div>

                <div className="mt-4 flex flex-wrap gap-2">
                  {trendData.series.map((series) => (
                    <span key={series.name} className="inline-flex items-center gap-2 rounded-md bg-surface-50 px-2 py-1 text-xs text-surface-600 dark:bg-surface-950 dark:text-surface-300">
                      <span className="h-2.5 w-2.5 rounded-sm" style={{ backgroundColor: series.color }} />
                      <span className="max-w-36 truncate" title={series.name}>{series.name}</span>
                      <span className="font-mono text-surface-500 dark:text-surface-400">
                        {formatTrendValue(series.value, trendMetric, true)}
                      </span>
                    </span>
                  ))}
                </div>
              </>
            )}
          </div>

          <div className="panel p-4">
            <div className="mb-3 flex items-center justify-between gap-3">
              <div className="flex items-center gap-2">
                <KeyRound className="h-4 w-4 text-amber-600 dark:text-amber-300" />
                <h2 className="text-sm font-semibold">小号排行</h2>
              </div>
              <span className="badge badge-neutral">{trendMetricLabels[trendMetric]}</span>
            </div>
            {trendData.rankings.length === 0 ? (
              <div className="flex h-56 items-center justify-center rounded-lg border border-dashed border-surface-300 text-sm text-surface-500 dark:border-surface-700 dark:text-surface-400">
                暂无排行数据
              </div>
            ) : (
              <div className="space-y-3">
                {trendData.rankings.slice(0, 8).map((item, index) => {
                  const color = trendColors[index % trendColors.length];
                  const percent = item.value > 0 ? Math.max(4, (item.value / rankingMax) * 100) : 2;
                  return (
                    <div key={item.name}>
                      <div className="mb-1.5 flex items-center justify-between gap-3 text-sm">
                        <span className="min-w-0 truncate font-medium text-surface-700 dark:text-surface-200" title={item.name}>
                          {item.name}
                        </span>
                        <span className="shrink-0 font-mono text-xs text-surface-500 dark:text-surface-400">
                          {formatTrendValue(item.value, trendMetric, true)}
                        </span>
                      </div>
                      <div className="h-1.5 overflow-hidden rounded-full bg-surface-100 dark:bg-surface-800">
                        <div className="h-full rounded-full" style={{ width: `${percent}%`, backgroundColor: color }} />
                      </div>
                      <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-surface-500 dark:text-surface-400">
                        <span>{item.requests} 次请求</span>
                        {item.errors > 0 && <span className="text-red-600 dark:text-red-400">{item.errors} 错误</span>}
                        <span>最近 {formatRelativeTime(item.lastTs)}</span>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </section>
      )}

      {loading ? (
        <div className="flex h-64 items-center justify-center">
          <div className="h-8 w-8 animate-spin rounded-full border-2 border-cyan-500 border-t-transparent" />
        </div>
      ) : logs.length === 0 ? (
        <div className="panel flex min-h-64 flex-col items-center justify-center p-8 text-center">
          <ScrollText className="mb-3 h-10 w-10 text-surface-300 dark:text-surface-700" />
          <p className="font-medium text-surface-800 dark:text-surface-200">
            暂无请求记录
          </p>
          <p className="mt-1 text-sm text-surface-500 dark:text-surface-400">
            发起 API 请求后，最近 1000 条记录会显示在这里
          </p>
        </div>
      ) : filteredLogs.length === 0 ? (
        <div className="panel flex min-h-64 flex-col items-center justify-center p-8 text-center">
          <ScrollText className="mb-3 h-10 w-10 text-surface-300 dark:text-surface-700" />
          <p className="font-medium text-surface-800 dark:text-surface-200">
            没有匹配的请求记录
          </p>
        </div>
      ) : (
        <section className="panel overflow-hidden">
          <div className="divide-y divide-surface-100 dark:divide-surface-800/60">
            {pagedLogs.map((log, index) => {
              const cost = costFor(log);
              return (
                <article
                  key={`${log.timestamp}-${log.path}-${index}`}
                  className="px-4 py-4 text-sm text-surface-700 transition-colors hover:bg-surface-50 dark:text-surface-200 dark:hover:bg-surface-950"
                >
                  <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
                    <span className={`badge ${statusClass(log.status)}`}>{log.status}</span>
                    <time className="font-mono text-xs text-surface-500 dark:text-surface-400">
                      {formatTime(log.timestamp)}
                    </time>
                    <span className="min-w-0 break-all font-mono text-xs text-surface-600 dark:text-surface-300">
                      {log.method} {log.path}
                    </span>
                    <span className="ml-auto whitespace-nowrap font-mono text-xs text-surface-500">
                      {log.duration_ms}ms
                    </span>
                  </div>

                  <div className="mt-3 grid grid-cols-1 gap-2 sm:grid-cols-3">
                    <div className="min-w-0 rounded-md bg-surface-50 px-3 py-2 dark:bg-surface-950">
                      <div className="metric-label">模型</div>
                      <div className="mt-1 break-all font-medium">{log.model || "—"}</div>
                    </div>
                    <div className="min-w-0 rounded-md bg-surface-50 px-3 py-2 dark:bg-surface-950">
                      <div className="metric-label">服务商</div>
                      <div className="mt-1 break-all font-medium">{log.provider || "—"}</div>
                    </div>
                    <div className="min-w-0 rounded-md bg-surface-50 px-3 py-2 dark:bg-surface-950">
                      <div className="metric-label">密钥</div>
                      <div className="mt-1 flex min-w-0 items-center gap-1 font-medium">
                        <KeyRound className="h-3 w-3 shrink-0 text-surface-400" />
                        <span className="break-all">{log.api_key_name || "—"}</span>
                      </div>
                    </div>
                  </div>

                  <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-3 xl:grid-cols-6">
                    <div className="rounded-md border border-surface-100 px-3 py-2 dark:border-surface-800">
                      <div className="metric-label">Input</div>
                      <div className="mt-1 inline-flex items-center gap-1 font-mono text-xs text-emerald-600 dark:text-emerald-300">
                        <ArrowDown className="h-3 w-3" />
                        {log.input_tokens > 0 ? formatTokens(log.input_tokens) : "—"}
                      </div>
                    </div>
                    <div className="rounded-md border border-surface-100 px-3 py-2 dark:border-surface-800">
                      <div className="metric-label">Output</div>
                      <div className="mt-1 inline-flex items-center gap-1 font-mono text-xs text-sky-600 dark:text-sky-300">
                        <ArrowUp className="h-3 w-3" />
                        {log.output_tokens > 0 ? formatTokens(log.output_tokens) : "—"}
                      </div>
                    </div>
                    <div className="rounded-md border border-surface-100 px-3 py-2 dark:border-surface-800">
                      <div className="metric-label">Cache Read</div>
                      <div className="mt-1 font-mono text-xs text-violet-600 dark:text-violet-300">
                        {(log.cache_read_tokens ?? log.cached_tokens) > 0
                          ? formatTokens(log.cache_read_tokens ?? log.cached_tokens)
                          : "—"}
                      </div>
                    </div>
                    <div className="rounded-md border border-surface-100 px-3 py-2 dark:border-surface-800">
                      <div className="metric-label">Cache Write</div>
                      <div className="mt-1 font-mono text-xs text-fuchsia-600 dark:text-fuchsia-300">
                        {(log.cache_write_tokens ?? 0) > 0
                          ? formatTokens(log.cache_write_tokens)
                          : "—"}
                      </div>
                    </div>
                    <div className="rounded-md border border-surface-100 px-3 py-2 dark:border-surface-800 sm:col-span-2 xl:col-span-2">
                      <div className="metric-label">费用</div>
                      <div className="mt-1 font-mono text-xs">
                        {cost ? (
                          <>
                            {formatMoney(cost.usd, "$")}
                            <span className="mx-1 text-surface-400">/</span>
                            {formatMoney(cost.cny, "¥")}
                          </>
                        ) : (
                          <span className="text-surface-400">未配置</span>
                        )}
                      </div>
                    </div>
                  </div>

                  {log.error && (
                    <div className="mt-2 break-words rounded-md border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
                      {log.error}
                    </div>
                  )}
                </article>
              );
            })}
          </div>
          <div className="flex flex-wrap items-center justify-between gap-3 border-t border-surface-200 px-4 py-3 text-sm dark:border-surface-800">
            <span className="text-surface-500 dark:text-surface-400">
              第 {safePage} / {pageCount} 页，每页 {pageSize} 条
            </span>
            <div className="flex items-center gap-2">
              <button className="btn-secondary" disabled={safePage <= 1} onClick={() => setPage((value) => Math.max(1, value - 1))}>
                上一页
              </button>
              <button className="btn-secondary" disabled={safePage >= pageCount} onClick={() => setPage((value) => Math.min(pageCount, value + 1))}>
                下一页
              </button>
            </div>
          </div>
        </section>
      )}
    </div>
  );
}
