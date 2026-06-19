import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Boxes, GripVertical, Loader2, Route, Search } from "lucide-react";

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
  providers: {
    id: string;
    name: string;
    protocol: Provider["protocol"];
    priority: number;
    enabled: boolean;
  }[];
}

export function reorderProvidersByRoute(
  providers: Provider[],
  routeProviderIds: string[],
  sourceId: string,
  targetId: string,
) {
  if (sourceId === targetId) return providers;

  const sourceIndex = routeProviderIds.indexOf(sourceId);
  const targetIndex = routeProviderIds.indexOf(targetId);
  if (sourceIndex < 0 || targetIndex < 0) return providers;

  const reorderedRouteIds = [...routeProviderIds];
  const [movedId] = reorderedRouteIds.splice(sourceIndex, 1);
  reorderedRouteIds.splice(targetIndex, 0, movedId);

  const originalIndex = new Map(providers.map((provider, index) => [provider.id, index]));
  const globallySorted = [...providers].sort(
    (a, b) =>
      a.priority - b.priority ||
      (originalIndex.get(a.id) ?? 0) - (originalIndex.get(b.id) ?? 0),
  );
  const routeProviderSet = new Set(routeProviderIds);
  const providerById = new Map(providers.map((provider) => [provider.id, provider]));
  let routeIndex = 0;
  const reorderedGlobal = globallySorted.map((provider) => {
    if (!routeProviderSet.has(provider.id)) return provider;
    const nextProvider = providerById.get(reorderedRouteIds[routeIndex]);
    routeIndex += 1;
    return nextProvider ?? provider;
  });
  const priorityById = new Map(
    reorderedGlobal.map((provider, priority) => [provider.id, priority]),
  );

  return providers.map((provider) => ({
    ...provider,
    priority: priorityById.get(provider.id) ?? provider.priority,
  }));
}

export default function Models() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [query, setQuery] = useState("");
  const [draggedRoute, setDraggedRoute] = useState<{
    modelName: string;
    providerId: string;
  } | null>(null);
  const [savingPriority, setSavingPriority] = useState(false);
  const [error, setError] = useState<string | null>(null);

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
          id: provider.id,
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

  const moveProvider = async (modelName: string, targetProviderId: string) => {
    if (
      !config ||
      !draggedRoute ||
      draggedRoute.modelName !== modelName ||
      draggedRoute.providerId === targetProviderId ||
      savingPriority
    ) {
      setDraggedRoute(null);
      return;
    }

    const model = models.find((item) => item.name === modelName);
    if (!model) {
      setDraggedRoute(null);
      return;
    }

    const previousConfig = config;
    const nextConfig = {
      ...config,
      providers: reorderProvidersByRoute(
        config.providers,
        model.providers.map((provider) => provider.id),
        draggedRoute.providerId,
        targetProviderId,
      ),
    };

    setDraggedRoute(null);
    setError(null);
    setSavingPriority(true);
    setConfig(nextConfig);
    try {
      const saved = await invoke<AppConfig>("save_config_cmd", { config: nextConfig });
      setConfig(saved);
    } catch (e) {
      console.error(e);
      setConfig(previousConfig);
      setError(`保存服务商优先级失败：${String(e)}`);
    } finally {
      setSavingPriority(false);
    }
  };

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

      <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-surface-200 bg-surface-50 px-4 py-3 text-sm text-surface-600 dark:border-surface-800 dark:bg-surface-900 dark:text-surface-300">
        <span>拖动路由即可调整服务商全局优先级，顺序会自动保存。</span>
        {savingPriority && (
          <span className="inline-flex items-center gap-2 text-cyan-700 dark:text-cyan-300">
            <Loader2 className="h-4 w-4 animate-spin" />
            正在保存
          </span>
        )}
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          {error}
        </div>
      )}

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
                        key={provider.id}
                        data-testid="provider-route"
                        data-provider-id={provider.id}
                        draggable={!savingPriority}
                        onDragStart={(event) => {
                          event.dataTransfer.effectAllowed = "move";
                          event.dataTransfer.setData("text/plain", provider.id);
                          setDraggedRoute({ modelName: model.name, providerId: provider.id });
                        }}
                        onDragOver={(event) => {
                          if (draggedRoute?.modelName === model.name && !savingPriority) {
                            event.preventDefault();
                            event.dataTransfer.dropEffect = "move";
                          }
                        }}
                        onDrop={(event) => {
                          event.preventDefault();
                          void moveProvider(model.name, provider.id);
                        }}
                        onDragEnd={() => setDraggedRoute(null)}
                        className={`flex items-center gap-3 rounded-lg border px-3 py-2 text-sm transition-colors ${
                          draggedRoute?.providerId === provider.id
                            ? "border-cyan-400 bg-cyan-50 opacity-60 dark:border-cyan-600 dark:bg-cyan-950/30"
                            : "cursor-grab border-transparent bg-surface-50 hover:border-surface-300 active:cursor-grabbing dark:bg-surface-950 dark:hover:border-surface-700"
                        }`}
                      >
                        <GripVertical
                          className="h-4 w-4 shrink-0 text-surface-400"
                          aria-label="拖动调整优先级"
                        />
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
