import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Activity, ArrowDown, ArrowUp, KeyRound, RefreshCw, ScrollText, Search, Trash2 } from "lucide-react";

interface RequestLogEntry {
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

interface ModelPrice {
  model: string;
  input_usd_per_million: number;
  output_usd_per_million: number;
  cached_usd_per_million: number;
}

interface AppConfig {
  providers: Provider[];
  model_prices: ModelPrice[];
  usd_to_cny_rate: number;
}

function formatTime(ts: number) {
  const d = new Date(ts * 1000);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function formatTokens(value: number) {
  return new Intl.NumberFormat("en-US").format(value);
}

function formatMoney(value: number, symbol: string) {
  if (value === 0) return `${symbol}0.0000`;
  return `${symbol}${value < 0.01 ? value.toFixed(6) : value.toFixed(4)}`;
}

function statusClass(status: number) {
  if (status >= 200 && status < 300) return "badge-success";
  if (status >= 400 && status < 500) return "badge-warning";
  return "badge-error";
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
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(1);
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
      await invoke("clear_request_logs");
      setLogs([]);
    } catch (e) {
      console.error(e);
    }
  };

  const modelOptions = Array.from(new Set(logs.map((log) => log.model).filter(Boolean))).sort();
  const providerOptions = Array.from(new Set(logs.map((log) => log.provider).filter(Boolean))).sort();
  const keyOptions = Array.from(new Set(logs.map((log) => log.api_key_name).filter(Boolean))).sort();

  const priceMap = new Map(
    (config?.model_prices ?? [])
      .filter((price) => price.model.trim())
      .map((price) => [price.model.toLowerCase(), price]),
  );

  const filteredLogs = logs.filter((log) => {
    if (modelFilter !== "all" && log.model !== modelFilter) return false;
    if (providerFilter !== "all" && log.provider !== providerFilter) return false;
    if (keyFilter !== "all" && log.api_key_name !== keyFilter) return false;
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
    const price = priceMap.get(log.model.toLowerCase());
    if (!price) return null;
    const usd =
      (log.input_tokens / 1_000_000) * price.input_usd_per_million +
      (log.output_tokens / 1_000_000) * price.output_usd_per_million +
      (log.cached_tokens / 1_000_000) * price.cached_usd_per_million;
    return {
      usd,
      cny: usd * (config?.usd_to_cny_rate ?? 7.2),
    };
  };

  const totals = filteredLogs.reduce(
    (acc, log) => {
      const cost = costFor(log);
      acc.input += log.input_tokens;
      acc.output += log.output_tokens;
      acc.cached += log.cached_tokens;
      acc.usd += cost?.usd ?? 0;
      return acc;
    },
    { input: 0, output: 0, cached: 0, usd: 0 },
  );

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
          <button className="btn-secondary" onClick={clearLogs} disabled={logs.length === 0}>
            <Trash2 className="h-4 w-4" />
            清空
          </button>
        </div>
      </div>

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
        <div className="mt-3 grid grid-cols-2 gap-2 text-sm lg:grid-cols-5">
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
            <div className="metric-label">Cache</div>
            <div className="mt-1 font-semibold text-violet-600 dark:text-violet-300">{formatTokens(totals.cached)}</div>
          </div>
          <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
            <div className="metric-label">Cost</div>
            <div className="mt-1 font-semibold">
              {formatMoney(totals.usd, "$")} / {formatMoney(totals.usd * (config?.usd_to_cny_rate ?? 7.2), "¥")}
            </div>
          </div>
        </div>
      </section>

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
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-surface-200 text-left text-xs uppercase tracking-wide text-surface-500 dark:border-surface-800 dark:text-surface-400">
                  <th className="px-3 py-2 font-medium">时间</th>
                  <th className="px-3 py-2 font-medium">路径</th>
                  <th className="px-3 py-2 font-medium">模型</th>
                  <th className="px-3 py-2 font-medium">服务商</th>
                  <th className="px-3 py-2 font-medium">密钥</th>
                  <th className="px-3 py-2 font-medium">状态</th>
                  <th className="px-3 py-2 text-right font-medium">耗时</th>
                  <th className="px-3 py-2 text-right font-medium">In</th>
                  <th className="px-3 py-2 text-right font-medium">Out</th>
                  <th className="px-3 py-2 text-right font-medium">Cache</th>
                  <th className="px-3 py-2 text-right font-medium">费用</th>
                  <th className="px-3 py-2 font-medium">错误</th>
                </tr>
              </thead>
              <tbody>
                {pagedLogs.map((log, index) => {
                  const cost = costFor(log);
                  return (
                  <tr
                    key={index}
                    className="border-b border-surface-100 text-surface-700 last:border-0 hover:bg-surface-50 dark:border-surface-800/60 dark:text-surface-200 dark:hover:bg-surface-950"
                  >
                    <td className="whitespace-nowrap px-3 py-2 font-mono text-xs text-surface-500 dark:text-surface-400">
                      {formatTime(log.timestamp)}
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 font-mono text-xs text-surface-500">
                      {log.path}
                    </td>
                    <td className="max-w-[16rem] truncate px-3 py-2" title={log.model}>
                      {log.model || "—"}
                    </td>
                    <td className="max-w-[10rem] truncate px-3 py-2" title={log.provider}>
                      {log.provider || "—"}
                    </td>
                    <td className="max-w-[10rem] truncate px-3 py-2" title={log.api_key_name}>
                      <span className="inline-flex items-center gap-1">
                        <KeyRound className="h-3 w-3 text-surface-400" />
                        {log.api_key_name || "—"}
                      </span>
                    </td>
                    <td className="px-3 py-2">
                      <span className={`badge ${statusClass(log.status)}`}>{log.status}</span>
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-right font-mono text-xs">
                      {log.duration_ms}ms
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-right font-mono text-xs">
                      {log.input_tokens > 0 ? (
                        <span className="inline-flex items-center gap-1 text-emerald-600 dark:text-emerald-300">
                          <ArrowDown className="h-3 w-3" />
                          {log.input_tokens}
                        </span>
                      ) : (
                        "—"
                      )}
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-right font-mono text-xs">
                      {log.output_tokens > 0 ? (
                        <span className="inline-flex items-center gap-1 text-sky-600 dark:text-sky-300">
                          <ArrowUp className="h-3 w-3" />
                          {log.output_tokens}
                        </span>
                      ) : (
                        "—"
                      )}
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-right font-mono text-xs">
                      {log.cached_tokens > 0 ? (
                        <span className="text-violet-600 dark:text-violet-300">
                          {log.cached_tokens}
                        </span>
                      ) : (
                        "—"
                      )}
                    </td>
                    <td className="whitespace-nowrap px-3 py-2 text-right font-mono text-xs">
                      {cost ? (
                        <span title={`${formatMoney(cost.usd, "$")} / ${formatMoney(cost.cny, "¥")}`}>
                          {formatMoney(cost.usd, "$")}
                          <span className="ml-1 text-surface-400">/</span>
                          <span className="ml-1">{formatMoney(cost.cny, "¥")}</span>
                        </span>
                      ) : (
                        <span className="text-surface-400">未配置</span>
                      )}
                    </td>
                    <td className="max-w-[20rem] truncate px-3 py-2 text-xs text-red-600 dark:text-red-400" title={log.error ?? ""}>
                      {log.error ?? ""}
                    </td>
                  </tr>
                  );
                })}
              </tbody>
            </table>
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
