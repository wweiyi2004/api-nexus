import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Activity,
  ArrowDown,
  ArrowUp,
  Boxes,
  CheckCircle2,
  Clipboard,
  Database,
  Hash,
  KeyRound,
  Network,
  Pause,
  Play,
  RefreshCw,
  Server,
  ShieldCheck,
  Split,
} from "lucide-react";

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

interface ServerStatus {
  running: boolean;
  port: number;
  host: string;
  url: string;
}

interface AppConfig {
  providers: Provider[];
  proxy_port: number;
  proxy_host: string;
  auto_start: boolean;
  proxy_api_key: string;
}

interface TokenStats {
  request_count: number;
  input_tokens: number;
  output_tokens: number;
  cached_tokens: number;
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

function unique<T>(items: T[]) {
  return Array.from(new Set(items));
}

function formatTokens(value: number) {
  return new Intl.NumberFormat("en-US").format(value);
}

function RunningWave({ active }: { active: boolean }) {
  return (
    <div
      className={`flex h-7 items-end gap-1 ${active ? "text-emerald-500 dark:text-emerald-300" : "text-surface-300 dark:text-surface-700"}`}
      aria-hidden="true"
    >
      {[14, 22, 18, 26, 16].map((height, index) => (
        <span
          key={index}
          className={`w-1.5 origin-bottom rounded-full bg-current ${active ? "animate-[running-wave_1.1s_ease-in-out_infinite]" : ""}`}
          style={{ height, animationDelay: `${index * 120}ms` }}
        />
      ))}
    </div>
  );
}

export default function Dashboard() {
  const [status, setStatus] = useState<ServerStatus | null>(null);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [tokenStats, setTokenStats] = useState<TokenStats | null>(null);
  const [logs, setLogs] = useState<RequestLogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  const fetchData = async () => {
    try {
      const [serverStatus, appConfig, usageStats, requestLogs] = await Promise.all([
        invoke<ServerStatus>("get_server_status"),
        invoke<AppConfig>("get_config"),
        invoke<TokenStats>("get_token_stats"),
        invoke<RequestLogEntry[]>("get_request_logs"),
      ]);
      setStatus(serverStatus);
      setConfig(appConfig);
      setTokenStats(usageStats);
      setLogs(requestLogs);
    } catch (e) {
      console.error(e);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 3000);
    return () => clearInterval(interval);
  }, []);

  const toggleServer = async () => {
    try {
      setError(null);
      if (status?.running) {
        await invoke("stop_proxy");
      } else {
        await invoke("start_proxy");
      }
      await fetchData();
    } catch (e) {
      console.error(e);
      setError(String(e));
    }
  };

  const resetTokenStats = async () => {
    try {
      setError(null);
      await invoke("reset_token_stats");
      await fetchData();
    } catch (e) {
      console.error(e);
      setError(String(e));
    }
  };

  const copy = async (value: string, key: string) => {
    await navigator.clipboard.writeText(value);
    setCopied(key);
    setTimeout(() => setCopied(null), 1400);
  };

  const stats = useMemo(() => {
    const providers = config?.providers ?? [];
    const enabled = providers.filter((provider) => provider.enabled);
    const models = unique(enabled.flatMap((provider) => provider.models));
    return {
      providers: providers.length,
      enabledProviders: enabled.length,
      openaiProviders: enabled.filter((provider) => provider.protocol !== "anthropic").length,
      anthropicProviders: enabled.filter((provider) => provider.protocol === "anthropic").length,
      models: models.length,
    };
  }, [config]);

  const perProvider = useMemo(() => {
    const map = new Map<string, { input: number; output: number; cached: number; count: number; errors: number }>();
    for (const log of logs) {
      const key = log.provider || "(未知)";
      const entry = map.get(key) ?? { input: 0, output: 0, cached: 0, count: 0, errors: 0 };
      entry.input += log.input_tokens;
      entry.output += log.output_tokens;
      entry.cached += log.cached_tokens;
      entry.count += 1;
      if (log.status >= 400) entry.errors += 1;
      map.set(key, entry);
    }
    return [...map.entries()].sort((a, b) => (b[1].input + b[1].output + b[1].cached) - (a[1].input + a[1].output + a[1].cached));
  }, [logs]);

  const baseUrl = status?.url ?? `http://${config?.proxy_host ?? "127.0.0.1"}:${config?.proxy_port ?? 11434}`;

  if (loading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-cyan-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="metric-label">Gateway Console</div>
          <h1 className="mt-1 text-2xl font-semibold text-surface-950 dark:text-white">
            本地 API 网关
          </h1>
        </div>
        <button
          onClick={toggleServer}
          className={status?.running ? "btn-danger" : "btn-primary"}
        >
          {status?.running ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
          {status?.running ? "停止代理" : "启动代理"}
        </button>
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          {error}
        </div>
      )}

      <div className="rounded-lg border border-surface-200 bg-surface-50 px-4 py-2 text-xs text-surface-500 dark:border-surface-800 dark:bg-surface-950 dark:text-surface-400">
            关闭窗口将最小化到系统托盘，代理继续后台运行。右键托盘图标可显示窗口或退出。
      </div>

      <section className="grid grid-cols-1 gap-3 md:grid-cols-4">
        <div
          className={`panel relative overflow-hidden p-4 ${
            status?.running
              ? "border-emerald-200 dark:border-emerald-500/40"
              : ""
          }`}
        >
          {status?.running && (
            <div className="absolute inset-x-0 top-0 h-0.5 overflow-hidden bg-emerald-100 dark:bg-emerald-500/10">
              <div className="h-full w-1/3 animate-[running-bar_1.6s_ease-in-out_infinite] bg-gradient-to-r from-transparent via-emerald-500 to-transparent" />
            </div>
          )}
          <div className="flex items-center justify-between">
            <div>
              <div className="metric-label">Status</div>
              <div className="mt-2 flex flex-wrap items-center gap-2">
                <span className="text-xl font-semibold">
                  {status?.running ? "运行中" : "已停止"}
                </span>
                {status?.running && (
                  <span className="badge badge-success">
                    <span className="relative flex h-2 w-2">
                      <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75" />
                      <span className="relative inline-flex h-2 w-2 rounded-full bg-emerald-500" />
                    </span>
                    Live
                  </span>
                )}
              </div>
              <div className="mt-2 text-xs text-surface-500 dark:text-surface-400">
                {status?.running ? `正在监听 ${baseUrl}` : "代理服务未启动"}
              </div>
            </div>
            <div
              className={`relative flex h-11 w-11 items-center justify-center rounded-lg ${
                status?.running
                  ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300"
                  : "bg-surface-100 text-surface-500 dark:bg-surface-800 dark:text-surface-400"
              }`}
            >
              {status?.running && (
                <span className="absolute inset-0 rounded-lg bg-emerald-400/30 animate-ping" />
              )}
              <Activity className="h-5 w-5" />
            </div>
          </div>
          <div className="mt-4 flex items-end justify-between gap-3">
            <RunningWave active={Boolean(status?.running)} />
            <span className="font-mono text-[11px] text-surface-500 dark:text-surface-400">
              {status?.running ? "accepting requests" : "idle"}
            </span>
          </div>
        </div>
        <div className="panel p-4">
          <div className="metric-label">Providers</div>
          <div className="mt-2 flex items-baseline gap-2">
            <span className="text-xl font-semibold">{stats.enabledProviders}</span>
            <span className="text-sm text-surface-500">/ {stats.providers}</span>
          </div>
          <div className="mt-3 flex flex-wrap gap-2">
            <span className="badge badge-neutral">OpenAI {stats.openaiProviders}</span>
            <span className="badge badge-neutral">Anthropic {stats.anthropicProviders}</span>
          </div>
        </div>
        <div className="panel p-4">
          <div className="metric-label">Models</div>
          <div className="mt-2 text-xl font-semibold">{stats.models}</div>
          <div className="mt-3 text-xs text-surface-500 dark:text-surface-400">
            去重后的可路由模型
          </div>
        </div>
        <div className="panel p-4">
          <div className="metric-label">Auth</div>
          <div className="mt-2 flex items-center gap-2 text-xl font-semibold">
            {config?.proxy_api_key ? "已保护" : "未设置"}
          </div>
          <div className="mt-3 flex items-center gap-1 text-xs text-surface-500 dark:text-surface-400">
            <ShieldCheck className="h-3.5 w-3.5" />
            {config?.proxy_api_key ? "统一代理密钥启用" : "请求无需验证"}
          </div>
        </div>
      </section>

      <section className="panel p-4">
        <div className="mb-3 flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <Database className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
            <h2 className="text-sm font-semibold">Token 用量</h2>
          </div>
          <button className="btn-secondary" onClick={resetTokenStats}>
            <RefreshCw className="h-4 w-4" />
            清零
          </button>
        </div>
        <div className="grid grid-cols-1 gap-3 md:grid-cols-4">
          <div className="rounded-lg border border-surface-200 bg-surface-50 p-3 dark:border-surface-800 dark:bg-surface-950">
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="metric-label">Input</div>
                <div className="mt-2 text-xl font-semibold">
                  {formatTokens(tokenStats?.input_tokens ?? 0)}
                </div>
              </div>
              <ArrowDown className="h-5 w-5 text-emerald-600 dark:text-emerald-300" />
            </div>
          </div>
          <div className="rounded-lg border border-surface-200 bg-surface-50 p-3 dark:border-surface-800 dark:bg-surface-950">
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="metric-label">Output</div>
                <div className="mt-2 text-xl font-semibold">
                  {formatTokens(tokenStats?.output_tokens ?? 0)}
                </div>
              </div>
              <ArrowUp className="h-5 w-5 text-sky-600 dark:text-sky-300" />
            </div>
          </div>
          <div className="rounded-lg border border-surface-200 bg-surface-50 p-3 dark:border-surface-800 dark:bg-surface-950">
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="metric-label">Cache</div>
                <div className="mt-2 text-xl font-semibold">
                  {formatTokens(tokenStats?.cached_tokens ?? 0)}
                </div>
              </div>
              <Database className="h-5 w-5 text-violet-600 dark:text-violet-300" />
            </div>
          </div>
          <div className="rounded-lg border border-surface-200 bg-surface-50 p-3 dark:border-surface-800 dark:bg-surface-950">
            <div className="flex items-center justify-between gap-3">
              <div>
                <div className="metric-label">Requests</div>
                <div className="mt-2 text-xl font-semibold">
                  {formatTokens(tokenStats?.request_count ?? 0)}
                </div>
              </div>
              <Hash className="h-5 w-5 text-amber-600 dark:text-amber-300" />
            </div>
          </div>
        </div>
      </section>

      {perProvider.length > 0 && (
        <section className="panel p-4">
          <div className="mb-3 flex items-center gap-2">
            <Network className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
            <h2 className="text-sm font-semibold">服务商用量</h2>
            <span className="ml-1 text-xs text-surface-500 dark:text-surface-400">
              基于最近 {logs.length} 条请求日志
            </span>
          </div>
          <div className="space-y-2">
            {perProvider.map(([name, usage]) => {
              const total = usage.input + usage.output + usage.cached;
              const maxTotal = perProvider[0][1].input + perProvider[0][1].output + perProvider[0][1].cached || 1;
              return (
                <div key={name} className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                  <div className="mb-1.5 flex items-center justify-between text-sm">
                    <span className="font-medium text-surface-700 dark:text-surface-200">{name}</span>
                    <span className="flex items-center gap-3 font-mono text-xs text-surface-500 dark:text-surface-400">
                      <span className="text-emerald-600 dark:text-emerald-300">↓{formatTokens(usage.input)}</span>
                      <span className="text-sky-600 dark:text-sky-300">↑{formatTokens(usage.output)}</span>
                      {usage.cached > 0 && <span className="text-violet-600 dark:text-violet-300">C{formatTokens(usage.cached)}</span>}
                      <span>{usage.count} 次</span>
                      {usage.errors > 0 && (
                        <span className="text-red-600 dark:text-red-400">{usage.errors} 错误</span>
                      )}
                    </span>
                  </div>
                  <div className="h-1.5 overflow-hidden rounded-full bg-surface-200 dark:bg-surface-800">
                    <div
                      className="h-full rounded-full bg-cyan-500"
                      style={{ width: `${(total / maxTotal) * 100}%` }}
                    />
                  </div>
                </div>
              );
            })}
          </div>
        </section>
      )}

      <section className="panel">
        <div className="flex items-center justify-between border-b border-surface-200 px-4 py-3 dark:border-surface-800">
          <div className="flex items-center gap-2">
            <Server className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
            <h2 className="text-sm font-semibold">入口地址</h2>
          </div>
          <span className={status?.running ? "badge badge-success" : "badge badge-neutral"}>
            {status?.running && (
              <span className="relative flex h-2 w-2">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75" />
                <span className="relative inline-flex h-2 w-2 rounded-full bg-emerald-500" />
              </span>
            )}
            {status?.running ? "Listening" : "Offline"}
          </span>
        </div>
        <div className="grid gap-3 p-4 lg:grid-cols-2">
          {[
            { label: "OpenAI Base URL", value: `${baseUrl}/v1`, key: "openai" },
            { label: "Anthropic Base URL", value: baseUrl, key: "anthropic" },
          ].map((item) => (
            <div key={item.key} className="rounded-lg border border-surface-200 bg-surface-50 p-3 dark:border-surface-800 dark:bg-surface-950">
              <div className="mb-2 flex items-center justify-between">
                <span className="text-xs font-medium text-surface-500 dark:text-surface-400">
                  {item.label}
                </span>
                <button
                  className="btn-icon"
                  onClick={() => copy(item.value, item.key)}
                  title="复制"
                >
                  {copied === item.key ? <CheckCircle2 className="h-4 w-4" /> : <Clipboard className="h-4 w-4" />}
                </button>
              </div>
              <code className="block break-all text-sm text-surface-800 dark:text-surface-100">
                {item.value}
              </code>
            </div>
          ))}
        </div>
      </section>

      <section className="grid grid-cols-1 gap-3 lg:grid-cols-2">
        <div className="panel p-4">
          <div className="mb-3 flex items-center gap-2">
            <Split className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
            <h2 className="text-sm font-semibold">协议入口</h2>
          </div>
          <div className="space-y-2 text-sm">
            <div className="flex items-center justify-between rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
              <span className="text-surface-600 dark:text-surface-300">OpenAI-compatible</span>
              <code className="text-xs text-surface-500">/v1/chat/completions</code>
            </div>
            <div className="flex items-center justify-between rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
              <span className="text-surface-600 dark:text-surface-300">Anthropic Messages</span>
              <code className="text-xs text-surface-500">/v1/messages</code>
            </div>
          </div>
        </div>
        <div className="panel p-4">
          <div className="mb-3 flex items-center gap-2">
            <KeyRound className="h-4 w-4 text-amber-600 dark:text-amber-300" />
            <h2 className="text-sm font-semibold">客户端配置</h2>
          </div>
          <div className="space-y-2 text-sm text-surface-600 dark:text-surface-300">
            <div className="flex items-center justify-between rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
              <span>Claude Code</span>
              <code className="text-xs text-surface-500">ANTHROPIC_BASE_URL={baseUrl}</code>
            </div>
            <div className="flex items-center justify-between rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
              <span>OpenAI SDK</span>
              <code className="text-xs text-surface-500">baseURL={baseUrl}/v1</code>
            </div>
          </div>
        </div>
      </section>

      {stats.models > 0 && (
        <section className="panel p-4">
          <div className="mb-3 flex items-center gap-2">
            <Boxes className="h-4 w-4 text-emerald-600 dark:text-emerald-300" />
            <h2 className="text-sm font-semibold">可用模型</h2>
          </div>
          <div className="flex flex-wrap gap-2">
            {unique(config?.providers.filter((provider) => provider.enabled).flatMap((provider) => provider.models) ?? []).map((model) => (
              <span key={model} className="badge badge-neutral">
                {model}
              </span>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
