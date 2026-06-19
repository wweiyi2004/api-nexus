import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { check, Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { CheckCircle2, CircleOff, Clipboard, Download, KeyRound, Power, RefreshCw, Save, ServerCog, Tags, Trash2, Plus } from "lucide-react";

interface ModelAlias {
  alias: string;
  model: string;
}

interface ProxyApiKey {
  id: string;
  name: string;
  key: string;
  enabled: boolean;
}

interface ModelPrice {
  model: string;
  input_usd_per_million: number;
  output_usd_per_million: number;
  cached_usd_per_million: number;
  cache_read_usd_per_million: number;
  cache_write_usd_per_million: number;
}

interface AppConfig {
  providers: unknown[];
  proxy_port: number;
  proxy_host: string;
  auto_start: boolean;
  proxy_api_key: string;
  proxy_api_keys: ProxyApiKey[];
  model_aliases: ModelAlias[];
  model_prices: ModelPrice[];
  usd_to_cny_rate: number;
  log_retention_days: number;
  max_log_entries: number;
}

const emptyAlias: ModelAlias = { alias: "", model: "" };
const emptyPrice: ModelPrice = {
  model: "",
  input_usd_per_million: 0,
  output_usd_per_million: 0,
  cached_usd_per_million: 0,
  cache_read_usd_per_million: 0,
  cache_write_usd_per_million: 0,
};

function generateProxyKey() {
  return `sk-nexus-${crypto.randomUUID().replaceAll("-", "")}`;
}

export default function Settings() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [newAlias, setNewAlias] = useState<ModelAlias>(emptyAlias);
  const [newPrice, setNewPrice] = useState<ModelPrice>(emptyPrice);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  const [updateStatus, setUpdateStatus] = useState<string>("");
  const [updating, setUpdating] = useState(false);

  useEffect(() => {
    (async () => {
      try {
        const c = await invoke<AppConfig>("get_config");
        setConfig(c);
      } catch (e) {
        console.error(e);
        setError(String(e));
      }
    })();
  }, []);

  const baseUrl = useMemo(() => {
    if (!config) return "";
    return `http://${config.proxy_host}:${config.proxy_port}`;
  }, [config]);

  const handleSave = async () => {
    if (!config) return;
    try {
      setError(null);
      const updated = await invoke<AppConfig>("save_config_cmd", { config });
      setConfig(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
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

  const checkForUpdates = async () => {
    try {
      setError(null);
      setUpdateStatus("正在检查更新…");
      const update = await check({ timeout: 30000 });
      setAvailableUpdate(update);
      setUpdateStatus(update ? `发现新版本 ${update.version}` : "当前已是最新版本");
    } catch (e) {
      console.error(e);
      setUpdateStatus("");
      setError(`检查更新失败：${String(e)}`);
    }
  };

  const installUpdate = async () => {
    if (!availableUpdate) return;
    try {
      setError(null);
      setUpdating(true);
      let downloaded = 0;
      let total = 0;
      await availableUpdate.downloadAndInstall((event) => {
        if (event.event === "Started") total = event.data.contentLength ?? 0;
        if (event.event === "Progress") downloaded += event.data.chunkLength;
        if (event.event === "Finished") setUpdateStatus("安装完成，正在重启…");
        else if (total > 0) setUpdateStatus(`正在下载 ${Math.round((downloaded / total) * 100)}%`);
        else setUpdateStatus("正在下载更新…");
      });
      await relaunch();
    } catch (e) {
      console.error(e);
      setError(`安装更新失败：${String(e)}`);
      setUpdating(false);
    }
  };

  if (!config) {
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
          <div className="metric-label">Gateway Settings</div>
          <h1 className="mt-1 text-2xl font-semibold text-surface-950 dark:text-white">
            设置
          </h1>
        </div>
        <button className="btn-primary" onClick={handleSave}>
          <Save className="h-4 w-4" />
          保存设置
        </button>
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          {error}
        </div>
      )}
      {saved && (
        <div className="rounded-lg border border-emerald-200 bg-emerald-50 px-4 py-3 text-sm text-emerald-700 dark:border-emerald-900/60 dark:bg-emerald-950/30 dark:text-emerald-300">
          设置已保存
        </div>
      )}

      <section className="grid grid-cols-1 gap-4 lg:grid-cols-3">
        <div className="panel lg:col-span-2">
          <div className="flex items-center gap-2 border-b border-surface-200 px-4 py-3 dark:border-surface-800">
            <ServerCog className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
            <h2 className="text-sm font-semibold">代理监听</h2>
          </div>
          <div className="grid grid-cols-1 gap-4 p-4 md:grid-cols-2">
            <label className="space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">主机地址</span>
              <input
                className="input-field"
                value={config.proxy_host}
                onChange={(e) => setConfig({ ...config, proxy_host: e.target.value })}
              />
            </label>
            <label className="space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">端口</span>
              <input
                className="input-field"
                type="number"
                min={1}
                max={65535}
                value={config.proxy_port}
                onChange={(e) =>
                  setConfig({ ...config, proxy_port: parseInt(e.target.value, 10) || 11434 })
                }
              />
            </label>
          </div>
        </div>

        <div className="panel">
          <div className="flex items-center gap-2 border-b border-surface-200 px-4 py-3 dark:border-surface-800">
            <Power className="h-4 w-4 text-emerald-600 dark:text-emerald-300" />
            <h2 className="text-sm font-semibold">启动</h2>
          </div>
          <div className="p-4">
            <button
              onClick={() => setConfig({ ...config, auto_start: !config.auto_start })}
              className="btn-secondary w-full justify-between"
            >
              <span>启动时自动开启代理</span>
              <span className={config.auto_start ? "badge badge-success" : "badge badge-neutral"}>
                {config.auto_start ? "开启" : "关闭"}
              </span>
            </button>
          </div>
        </div>
      </section>

      <section className="panel">
        <div className="flex items-center gap-2 border-b border-surface-200 px-4 py-3 dark:border-surface-800">
          <ServerCog className="h-4 w-4 text-violet-600 dark:text-violet-300" />
          <h2 className="text-sm font-semibold">日志持久化</h2>
        </div>
        <div className="grid grid-cols-1 gap-4 p-4 md:grid-cols-2">
          <label className="space-y-1.5">
            <span className="text-sm font-medium text-surface-700 dark:text-surface-300">保留天数</span>
            <input
              className="input-field"
              type="number"
              min={1}
              max={3650}
              value={config.log_retention_days}
              onChange={(e) => setConfig({ ...config, log_retention_days: parseInt(e.target.value, 10) || 30 })}
            />
          </label>
          <label className="space-y-1.5">
            <span className="text-sm font-medium text-surface-700 dark:text-surface-300">最多记录数</span>
            <input
              className="input-field"
              type="number"
              min={100}
              max={1000000}
              value={config.max_log_entries}
              onChange={(e) => setConfig({ ...config, max_log_entries: parseInt(e.target.value, 10) || 10000 })}
            />
          </label>
          <p className="text-xs text-surface-500 dark:text-surface-400 md:col-span-2">
            请求日志保存在本机 SQLite 数据库；API 密钥使用当前 Windows 用户的 DPAPI 加密后单独保存。
          </p>
        </div>
      </section>

      <section className="panel">
        <div className="flex items-center gap-2 border-b border-surface-200 px-4 py-3 dark:border-surface-800">
          <RefreshCw className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
          <h2 className="text-sm font-semibold">应用更新</h2>
        </div>
        <div className="flex flex-wrap items-center justify-between gap-3 p-4">
          <div>
            <p className="text-sm text-surface-700 dark:text-surface-200">{updateStatus || "从 GitHub Release 检查签名更新"}</p>
            {availableUpdate?.body && (
              <p className="mt-1 max-w-2xl whitespace-pre-wrap text-xs text-surface-500 dark:text-surface-400">{availableUpdate.body}</p>
            )}
          </div>
          <div className="flex gap-2">
            <button className="btn-secondary" onClick={checkForUpdates} disabled={updating}>
              <RefreshCw className="h-4 w-4" />
              检查更新
            </button>
            {availableUpdate && (
              <button className="btn-primary" onClick={installUpdate} disabled={updating}>
                <Download className="h-4 w-4" />
                {updating ? "正在安装" : `安装 ${availableUpdate.version}`}
              </button>
            )}
          </div>
        </div>
      </section>

      {!(["127.0.0.1", "localhost", "::1"].includes(config.proxy_host.trim().toLowerCase())) && (
        <div className="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
          当前监听地址可能暴露到局域网。请确保客户端密钥已启用，并通过防火墙限制访问范围。
        </div>
      )}

      <section className="panel">
        <div className="flex items-center justify-between gap-3 border-b border-surface-200 px-4 py-3 dark:border-surface-800">
          <div className="flex items-center gap-2">
            <KeyRound className="h-4 w-4 text-amber-600 dark:text-amber-300" />
            <h2 className="text-sm font-semibold">客户端 API 密钥</h2>
          </div>
          <button
            className="btn-secondary"
            onClick={() =>
              setConfig({
                ...config,
                proxy_api_keys: [
                  ...config.proxy_api_keys,
                  {
                    id: crypto.randomUUID(),
                    name: `密钥 ${config.proxy_api_keys.length + 1}`,
                    key: generateProxyKey(),
                    enabled: true,
                  },
                ],
              })
            }
          >
            <Plus className="h-4 w-4" />
            新增密钥
          </button>
        </div>
        <div className="space-y-2 p-4">
          {config.proxy_api_keys.map((apiKey, index) => (
            <div key={apiKey.id || index} className="grid grid-cols-1 gap-2 rounded-lg border border-surface-200 p-3 dark:border-surface-800 lg:grid-cols-[14rem_1fr_auto_auto_auto]">
              <input
                className="input-field"
                placeholder="备注名称"
                value={apiKey.name}
                onChange={(e) => {
                  const next = [...config.proxy_api_keys];
                  next[index] = { ...apiKey, name: e.target.value };
                  setConfig({ ...config, proxy_api_keys: next });
                }}
              />
              <input
                className="input-field font-mono"
                type="password"
                value={apiKey.key}
                onChange={(e) => {
                  const next = [...config.proxy_api_keys];
                  next[index] = { ...apiKey, key: e.target.value };
                  setConfig({ ...config, proxy_api_keys: next });
                }}
              />
              <button
                className="btn-secondary"
                onClick={() => {
                  const next = [...config.proxy_api_keys];
                  next[index] = { ...apiKey, enabled: !apiKey.enabled };
                  setConfig({ ...config, proxy_api_keys: next });
                }}
              >
                {apiKey.enabled ? <CheckCircle2 className="h-4 w-4" /> : <CircleOff className="h-4 w-4" />}
                {apiKey.enabled ? "启用" : "禁用"}
              </button>
              <button
                className="btn-icon"
                title="复制密钥"
                onClick={() => copy(apiKey.key, `proxy-key-${apiKey.id || index}`)}
                disabled={!apiKey.key}
              >
                {copied === `proxy-key-${apiKey.id || index}` ? <CheckCircle2 className="h-4 w-4" /> : <Clipboard className="h-4 w-4" />}
              </button>
              <button
                className="btn-icon"
                title="删除"
                disabled={config.proxy_api_keys.length <= 1}
                onClick={() => {
                  const next = config.proxy_api_keys.filter((_, i) => i !== index);
                  setConfig({ ...config, proxy_api_keys: next });
                }}
              >
                <Trash2 className="h-4 w-4" />
              </button>
            </div>
          ))}
          <div className="text-xs text-surface-500 dark:text-surface-400">
            请求命中的密钥备注会写入请求日志，便于按客户端或用途筛选。
          </div>
        </div>
      </section>

      <section className="panel">
        <div className="flex items-center gap-2 border-b border-surface-200 px-4 py-3 dark:border-surface-800">
          <Tags className="h-4 w-4 text-violet-600 dark:text-violet-300" />
          <h2 className="text-sm font-semibold">模型别名</h2>
          <span className="ml-2 text-xs text-surface-500 dark:text-surface-400">
            请求中的别名会自动替换为真实模型名
          </span>
        </div>
        <div className="space-y-2 p-4">
          {config.model_aliases.map((alias, index) => (
            <div key={index} className="flex items-center gap-2">
              <input
                className="input-field"
                placeholder="别名 (如 fast)"
                value={alias.alias}
                onChange={(e) => {
                  const next = [...config.model_aliases];
                  next[index] = { ...alias, alias: e.target.value };
                  setConfig({ ...config, model_aliases: next });
                }}
              />
              <span className="shrink-0 text-sm text-surface-400">→</span>
              <input
                className="input-field"
                placeholder="真实模型名 (如 deepseek-v4-flash)"
                value={alias.model}
                onChange={(e) => {
                  const next = [...config.model_aliases];
                  next[index] = { ...alias, model: e.target.value };
                  setConfig({ ...config, model_aliases: next });
                }}
              />
              <button
                className="btn-icon shrink-0"
                title="删除"
                onClick={() => {
                  const next = config.model_aliases.filter((_, i) => i !== index);
                  setConfig({ ...config, model_aliases: next });
                }}
              >
                <Trash2 className="h-4 w-4" />
              </button>
            </div>
          ))}
          {config.model_aliases.length === 0 && (
            <div className="py-2 text-center text-xs text-surface-400">
              暂无别名。例如：fast → deepseek-v4-flash，sonnet → claude-sonnet-4-20250514
            </div>
          )}
          <div className="flex items-center gap-2 pt-2">
            <input
              className="input-field"
              placeholder="新别名"
              value={newAlias.alias}
              onChange={(e) => setNewAlias({ ...newAlias, alias: e.target.value })}
            />
            <span className="shrink-0 text-sm text-surface-400">→</span>
            <input
              className="input-field"
              placeholder="真实模型名"
              value={newAlias.model}
              onChange={(e) => setNewAlias({ ...newAlias, model: e.target.value })}
              onKeyDown={(e) => {
                if (e.key === "Enter" && newAlias.alias.trim() && newAlias.model.trim()) {
                  e.preventDefault();
                  setConfig({
                    ...config,
                    model_aliases: [...config.model_aliases, { ...newAlias }],
                  });
                  setNewAlias(emptyAlias);
                }
              }}
            />
            <button
              className="btn-secondary shrink-0"
              disabled={!newAlias.alias.trim() || !newAlias.model.trim()}
              onClick={() => {
                if (newAlias.alias.trim() && newAlias.model.trim()) {
                  setConfig({
                    ...config,
                    model_aliases: [...config.model_aliases, { ...newAlias }],
                  });
                  setNewAlias(emptyAlias);
                }
              }}
            >
              <Plus className="h-4 w-4" />
              添加
            </button>
          </div>
        </div>
      </section>

      <section className="panel">
        <div className="flex items-center justify-between gap-3 border-b border-surface-200 px-4 py-3 dark:border-surface-800">
          <div className="flex items-center gap-2">
            <Tags className="h-4 w-4 text-emerald-600 dark:text-emerald-300" />
            <h2 className="text-sm font-semibold">模型价格</h2>
            <span className="ml-2 text-xs text-surface-500 dark:text-surface-400">
              单位为美元 / 100 万 tokens
            </span>
          </div>
          <label className="flex items-center gap-2 text-xs text-surface-500 dark:text-surface-400">
            USD/CNY
            <input
              className="input-field w-24"
              type="number"
              step="0.01"
              min="0"
              value={config.usd_to_cny_rate}
              onChange={(e) =>
                setConfig({ ...config, usd_to_cny_rate: parseFloat(e.target.value) || 7.2 })
              }
            />
          </label>
        </div>
        <div className="space-y-2 p-4">
          {config.model_prices.map((price, index) => (
            <div key={`${price.model}-${index}`} className="grid grid-cols-1 gap-2 lg:grid-cols-[1fr_repeat(4,8rem)_auto]">
              <input
                className="input-field"
                placeholder="模型名"
                value={price.model}
                onChange={(e) => {
                  const next = [...config.model_prices];
                  next[index] = { ...price, model: e.target.value };
                  setConfig({ ...config, model_prices: next });
                }}
              />
              <input
                className="input-field"
                type="number"
                min="0"
                step="0.0001"
                placeholder="Input"
                value={price.input_usd_per_million}
                onChange={(e) => {
                  const next = [...config.model_prices];
                  next[index] = { ...price, input_usd_per_million: parseFloat(e.target.value) || 0 };
                  setConfig({ ...config, model_prices: next });
                }}
              />
              <input
                className="input-field"
                type="number"
                min="0"
                step="0.0001"
                placeholder="Output"
                value={price.output_usd_per_million}
                onChange={(e) => {
                  const next = [...config.model_prices];
                  next[index] = { ...price, output_usd_per_million: parseFloat(e.target.value) || 0 };
                  setConfig({ ...config, model_prices: next });
                }}
              />
              <input
                className="input-field"
                type="number"
                min="0"
                step="0.0001"
                placeholder="Cache Read"
                value={price.cache_read_usd_per_million}
                onChange={(e) => {
                  const next = [...config.model_prices];
                  const value = parseFloat(e.target.value) || 0;
                  next[index] = { ...price, cache_read_usd_per_million: value, cached_usd_per_million: value };
                  setConfig({ ...config, model_prices: next });
                }}
              />
              <input
                className="input-field"
                type="number"
                min="0"
                step="0.0001"
                placeholder="Cache Write"
                value={price.cache_write_usd_per_million}
                onChange={(e) => {
                  const next = [...config.model_prices];
                  next[index] = { ...price, cache_write_usd_per_million: parseFloat(e.target.value) || 0 };
                  setConfig({ ...config, model_prices: next });
                }}
              />
              <button
                className="btn-icon"
                title="删除"
                onClick={() => {
                  const next = config.model_prices.filter((_, i) => i !== index);
                  setConfig({ ...config, model_prices: next });
                }}
              >
                <Trash2 className="h-4 w-4" />
              </button>
            </div>
          ))}
          {config.model_prices.length === 0 && (
            <div className="py-2 text-center text-xs text-surface-400">
              暂无价格。添加后请求日志会自动计算每条请求费用。
            </div>
          )}
          <div className="grid grid-cols-1 gap-2 pt-2 lg:grid-cols-[1fr_repeat(4,8rem)_auto]">
            <input
              className="input-field"
              placeholder="模型名"
              value={newPrice.model}
              onChange={(e) => setNewPrice({ ...newPrice, model: e.target.value })}
            />
            <input
              className="input-field"
              type="number"
              min="0"
              step="0.0001"
              placeholder="Input"
              value={newPrice.input_usd_per_million}
              onChange={(e) => setNewPrice({ ...newPrice, input_usd_per_million: parseFloat(e.target.value) || 0 })}
            />
            <input
              className="input-field"
              type="number"
              min="0"
              step="0.0001"
              placeholder="Output"
              value={newPrice.output_usd_per_million}
              onChange={(e) => setNewPrice({ ...newPrice, output_usd_per_million: parseFloat(e.target.value) || 0 })}
            />
            <input
              className="input-field"
              type="number"
              min="0"
              step="0.0001"
              placeholder="Cache Read"
              value={newPrice.cache_read_usd_per_million}
              onChange={(e) => {
                const value = parseFloat(e.target.value) || 0;
                setNewPrice({ ...newPrice, cache_read_usd_per_million: value, cached_usd_per_million: value });
              }}
            />
            <input
              className="input-field"
              type="number"
              min="0"
              step="0.0001"
              placeholder="Cache Write"
              value={newPrice.cache_write_usd_per_million}
              onChange={(e) => setNewPrice({ ...newPrice, cache_write_usd_per_million: parseFloat(e.target.value) || 0 })}
            />
            <button
              className="btn-secondary"
              disabled={!newPrice.model.trim()}
              onClick={() => {
                if (!newPrice.model.trim()) return;
                setConfig({
                  ...config,
                  model_prices: [...config.model_prices, { ...newPrice }],
                });
                setNewPrice(emptyPrice);
              }}
            >
              <Plus className="h-4 w-4" />
              添加
            </button>
          </div>
        </div>
      </section>

      <section className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        {[
          {
            key: "openai",
            title: "OpenAI-compatible 客户端",
            lines: [
              `Base URL: ${baseUrl}/v1`,
              "Endpoint: /chat/completions",
              "Header: Authorization: Bearer <API_NEXUS_KEY>",
            ],
          },
          {
            key: "anthropic",
            title: "Claude Code / Anthropic 客户端",
            lines: [
              `ANTHROPIC_BASE_URL=${baseUrl}`,
              "ANTHROPIC_API_KEY=<API_NEXUS_KEY>",
              "Endpoint: /v1/messages",
            ],
          },
        ].map((block) => (
          <div key={block.key} className="panel">
            <div className="flex items-center justify-between border-b border-surface-200 px-4 py-3 dark:border-surface-800">
              <h2 className="text-sm font-semibold">{block.title}</h2>
              <button
                className="btn-icon"
                title="复制"
                onClick={() => copy(block.lines.join("\n"), block.key)}
              >
                {copied === block.key ? <CheckCircle2 className="h-4 w-4" /> : <Clipboard className="h-4 w-4" />}
              </button>
            </div>
            <pre className="overflow-x-auto p-4 text-xs leading-6 text-surface-600 dark:text-surface-300">
              {block.lines.join("\n")}
            </pre>
          </div>
        ))}
      </section>
    </div>
  );
}
