import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Boxes, Route, Search } from "lucide-react";

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

interface AppConfig {
  providers: Provider[];
  proxy_port: number;
  proxy_host: string;
  auto_start: boolean;
}

interface ModelInfo {
  name: string;
  providers: { name: string; protocol: Provider["protocol"]; priority: number; enabled: boolean }[];
}

export default function Models() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    (async () => {
      try {
        const c = await invoke<AppConfig>("get_config");
        setConfig(c);
      } catch (e) {
        console.error(e);
      }
    })();
  }, []);

  const models = useMemo(() => {
    const modelMap = new Map<string, ModelInfo>();
    for (const provider of config?.providers ?? []) {
      for (const model of provider.models) {
        if (!modelMap.has(model)) {
          modelMap.set(model, { name: model, providers: [] });
        }
        modelMap.get(model)!.providers.push({
          name: provider.name,
          protocol: provider.protocol,
          priority: provider.priority,
          enabled: provider.enabled,
        });
      }
    }

    return [...modelMap.values()]
      .map((model) => ({
        ...model,
        providers: model.providers.sort((a, b) => a.priority - b.priority),
      }))
      .filter((model) => model.name.toLowerCase().includes(query.toLowerCase()));
  }, [config, query]);

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="metric-label">Route Matrix</div>
          <h1 className="mt-1 text-2xl font-semibold text-surface-950 dark:text-white">
            模型路由
          </h1>
        </div>
        <label className="relative w-full max-w-sm">
          <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-surface-400" />
          <input
            className="input-field pl-9"
            placeholder="搜索模型"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
        </label>
      </div>

      {models.length > 0 ? (
        <section className="space-y-2">
          {models.map((model) => {
            const enabledCount = model.providers.filter((provider) => provider.enabled).length;
            return (
              <div key={model.name} className="data-row p-4">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <Boxes className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
                      <h3 className="break-all font-semibold text-surface-950 dark:text-white">
                        {model.name}
                      </h3>
                    </div>
                    <div className="mt-2 flex flex-wrap gap-2">
                      <span className="badge badge-neutral">{model.providers.length} 条路由</span>
                      <span className={enabledCount > 0 ? "badge badge-success" : "badge badge-warning"}>
                        {enabledCount} 可用
                      </span>
                    </div>
                  </div>

                  <div className="min-w-[360px] flex-1 space-y-2">
                    {model.providers.map((provider, index) => (
                      <div
                        key={`${provider.name}-${provider.priority}-${index}`}
                        className="flex items-center gap-3 rounded-lg bg-surface-50 px-3 py-2 text-sm dark:bg-surface-950"
                      >
                        <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-white text-xs font-semibold text-surface-600 ring-1 ring-surface-200 dark:bg-surface-900 dark:text-surface-300 dark:ring-surface-700">
                          {index + 1}
                        </span>
                        <Route className="h-4 w-4 shrink-0 text-surface-400" />
                        <span className="min-w-0 flex-1 truncate text-surface-700 dark:text-surface-200">
                          {provider.name || "未命名服务商"}
                        </span>
                        <span className="badge badge-info">
                          {provider.protocol === "anthropic" ? "Anthropic" : "OpenAI"}
                        </span>
                        <span className={provider.enabled ? "badge badge-success" : "badge badge-neutral"}>
                          {provider.enabled ? "启用" : "禁用"}
                        </span>
                      </div>
                    ))}
                  </div>
                </div>
              </div>
            );
          })}
        </section>
      ) : (
        <div className="panel flex min-h-64 flex-col items-center justify-center p-8 text-center">
          <Boxes className="mb-3 h-10 w-10 text-surface-300 dark:text-surface-700" />
          <p className="font-medium text-surface-800 dark:text-surface-200">
            没有匹配的模型
          </p>
        </div>
      )}
    </div>
  );
}
