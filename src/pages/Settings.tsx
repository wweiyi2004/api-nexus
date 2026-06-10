import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CheckCircle2, Clipboard, KeyRound, Power, Save, ServerCog } from "lucide-react";

interface AppConfig {
  providers: unknown[];
  proxy_port: number;
  proxy_host: string;
  auto_start: boolean;
  proxy_api_key: string;
}

export default function Settings() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

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
          <KeyRound className="h-4 w-4 text-amber-600 dark:text-amber-300" />
          <h2 className="text-sm font-semibold">统一 API 密钥</h2>
        </div>
        <div className="grid grid-cols-1 gap-4 p-4 lg:grid-cols-[1fr_auto_auto]">
          <input
            className="input-field"
            type="password"
            placeholder="留空保存将自动生成随机密钥"
            value={config.proxy_api_key}
            onChange={(e) => setConfig({ ...config, proxy_api_key: e.target.value })}
          />
          <button
            className="btn-icon self-center"
            title="复制密钥"
            onClick={() => copy(config.proxy_api_key, "proxy-key")}
            disabled={!config.proxy_api_key}
          >
            {copied === "proxy-key" ? <CheckCircle2 className="h-4 w-4" /> : <Clipboard className="h-4 w-4" />}
          </button>
          <span className={config.proxy_api_key ? "badge badge-success self-center" : "badge badge-warning self-center"}>
            {config.proxy_api_key ? "已启用" : "保存后自动生成"}
          </span>
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
