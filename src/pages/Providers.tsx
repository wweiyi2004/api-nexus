import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  CheckCircle2,
  CircleOff,
  FlaskConical,
  ListChecks,
  Loader2,
  Network,
  Pencil,
  Plus,
  Save,
  Trash2,
  X,
} from "lucide-react";

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

const emptyProvider: Provider = {
  id: "",
  name: "",
  protocol: "openai",
  base_url: "",
  api_key: "",
  models: [],
  enabled: true,
  priority: 0,
};

const providerPresets: Array<{
  id: string;
  label: string;
  name: string;
  protocol: Provider["protocol"];
  base_url: string;
  models: string[];
}> = [
  {
    id: "deepseek-openai",
    label: "DeepSeek · OpenAI",
    name: "DeepSeek",
    protocol: "openai",
    base_url: "https://api.deepseek.com",
    models: ["deepseek-v4-flash", "deepseek-v4-pro"],
  },
  {
    id: "deepseek-anthropic",
    label: "DeepSeek · Anthropic",
    name: "DeepSeek Anthropic",
    protocol: "anthropic",
    base_url: "https://api.deepseek.com/anthropic",
    models: ["deepseek-v4-flash", "deepseek-v4-pro"],
  },
  {
    id: "openai",
    label: "OpenAI",
    name: "OpenAI",
    protocol: "openai",
    base_url: "https://api.openai.com",
    models: ["gpt-4o"],
  },
  {
    id: "anthropic",
    label: "Anthropic",
    name: "Anthropic",
    protocol: "anthropic",
    base_url: "https://api.anthropic.com",
    models: [],
  },
  {
    id: "gemini",
    label: "Gemini · OpenAI",
    name: "Gemini",
    protocol: "openai",
    base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
    models: ["gemini-2.5-flash"],
  },
  {
    id: "mistral",
    label: "Mistral",
    name: "Mistral",
    protocol: "openai",
    base_url: "https://api.mistral.ai/v1",
    models: ["mistral-large-latest"],
  },
  {
    id: "xai",
    label: "xAI",
    name: "xAI",
    protocol: "openai",
    base_url: "https://api.x.ai/v1",
    models: [],
  },
  {
    id: "kimi",
    label: "Kimi",
    name: "Kimi",
    protocol: "openai",
    base_url: "https://api.moonshot.ai/v1",
    models: [],
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    name: "OpenRouter",
    protocol: "openai",
    base_url: "https://openrouter.ai/api/v1",
    models: [],
  },
  {
    id: "siliconflow",
    label: "SiliconFlow",
    name: "SiliconFlow",
    protocol: "openai",
    base_url: "https://api.siliconflow.cn/v1",
    models: [],
  },
  {
    id: "volcengine",
    label: "火山方舟",
    name: "火山方舟",
    protocol: "openai",
    base_url: "https://ark.cn-beijing.volces.com/api/v3",
    models: [],
  },
  {
    id: "zhipu",
    label: "智谱 GLM",
    name: "智谱 GLM",
    protocol: "openai",
    base_url: "https://open.bigmodel.cn/api/paas/v4",
    models: ["glm-5.1"],
  },
  {
    id: "hunyuan",
    label: "腾讯混元",
    name: "腾讯混元",
    protocol: "openai",
    base_url: "https://api.hunyuan.cloud.tencent.com/v1",
    models: [],
  },
  {
    id: "qianfan",
    label: "百度千帆",
    name: "百度千帆",
    protocol: "openai",
    base_url: "https://api.baiduqianfan.ai/v1",
    models: [],
  },
];

function protocolLabel(protocol: Provider["protocol"]) {
  return protocol === "anthropic" ? "Anthropic" : "OpenAI";
}

function responseErrorMessage(body: unknown) {
  if (!body || typeof body !== "object") return null;
  const record = body as Record<string, unknown>;
  const error = record.error;
  if (typeof error === "string") return error;
  if (error && typeof error === "object") {
    const message = (error as Record<string, unknown>).message;
    if (typeof message === "string") return message;
  }
  if (typeof record.message === "string") return record.message;
  return null;
}

interface ProviderTestResponse {
  status: number;
  success: boolean;
  body?: unknown;
  model?: string | null;
}

interface ProviderTestResult {
  success: boolean;
  message: string;
}

function testMessage(result: ProviderTestResponse) {
  const detail = responseErrorMessage(result.body);
  return result.success
    ? `通过 ${result.status}${result.model ? ` · ${result.model}` : ""}`
    : `失败 ${result.status}${detail ? ` · ${detail}` : ""}`;
}

export default function Providers() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [editing, setEditing] = useState<Provider | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [modelInput, setModelInput] = useState("");
  const [testing, setTesting] = useState<string | null>(null);
  const [testResults, setTestResults] = useState<Record<string, ProviderTestResult>>({});
  const [batchResult, setBatchResult] = useState<ProviderTestResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const fetchConfig = async () => {
    try {
      const c = await invoke<AppConfig>("get_config");
      setConfig(c);
    } catch (e) {
      console.error(e);
      setError(String(e));
    }
  };

  useEffect(() => {
    fetchConfig();
  }, []);

  const beginCreate = () => {
    setEditing({ ...emptyProvider });
    setModelInput("");
    setShowForm(true);
  };

  const beginEdit = (provider: Provider) => {
    setEditing({ ...provider });
    setModelInput("");
    setShowForm(true);
  };

  const applyPreset = (presetId: string) => {
    if (!editing || !presetId) return;
    const preset = providerPresets.find((item) => item.id === presetId);
    if (!preset) return;
    setEditing({
      ...editing,
      name: preset.name,
      protocol: preset.protocol,
      base_url: preset.base_url,
      models: preset.models,
    });
    setModelInput("");
  };

  const handleSave = async () => {
    if (!editing) return;
    try {
      setError(null);
      if (editing.id) {
        await invoke("update_provider", { provider: editing });
      } else {
        await invoke("add_provider", {
          provider: { ...editing, id: crypto.randomUUID() },
        });
      }
      setShowForm(false);
      setEditing(null);
      await fetchConfig();
    } catch (e) {
      console.error(e);
      setError(String(e));
    }
  };

  const handleDelete = async (id: string) => {
    try {
      setError(null);
      await invoke("remove_provider", { id });
      await fetchConfig();
    } catch (e) {
      console.error(e);
      setError(String(e));
    }
  };

  const handleToggle = async (provider: Provider) => {
    try {
      setError(null);
      await invoke("update_provider", {
        provider: { ...provider, enabled: !provider.enabled },
      });
      await fetchConfig();
    } catch (e) {
      console.error(e);
      setError(String(e));
    }
  };

  const testProvider = async (provider: Provider, model?: string) => {
    return invoke<ProviderTestResponse>("test_provider", {
      provider,
      model: model ?? null,
    });
  };

  const setProviderTestResult = (providerId: string, result: ProviderTestResult) => {
    setTestResults((current) => ({
      ...current,
      [providerId]: result,
    }));
  };

  const handleTest = async (provider: Provider) => {
    setTesting(provider.id);
    try {
      setError(null);
      setBatchResult(null);
      const result = await testProvider(provider);
      setProviderTestResult(provider.id, {
        success: result.success,
        message: `连接${testMessage(result)}`,
      });
    } catch (e) {
      setProviderTestResult(provider.id, { success: false, message: String(e) });
    } finally {
      setTesting(null);
    }
  };

  const handleTestModels = async (provider: Provider) => {
    const key = `${provider.id}:models`;
    setTesting(key);
    try {
      setError(null);
      setBatchResult(null);
      if (provider.models.length === 0) {
        const result = await testProvider(provider);
        setProviderTestResult(provider.id, {
          success: result.success,
          message: `模型列表${testMessage(result)}`,
        });
        return;
      }

      const outcomes = await Promise.all(
        provider.models.map(async (model) => {
          try {
            const result = await testProvider(provider, model);
            return result.success ? null : model;
          } catch {
            return model;
          }
        }),
      );
      const failed = outcomes.filter((model): model is string => model !== null);
      const passed = provider.models.length - failed.length;

      const allPassed = failed.length === 0;
      const failedText = failed.length > 0 ? ` · 失败 ${failed.slice(0, 3).join(", ")}` : "";
      const moreText = failed.length > 3 ? ` 等 ${failed.length} 个` : "";
      setProviderTestResult(provider.id, {
        success: allPassed,
        message: `模型 ${passed}/${provider.models.length} 通过${failedText}${moreText}`,
      });
    } catch (e) {
      setProviderTestResult(provider.id, { success: false, message: String(e) });
    } finally {
      setTesting(null);
    }
  };

  const handleTestAllProviders = async () => {
    const providers = config?.providers ?? [];
    if (providers.length === 0) return;

    setTesting("all-providers");
    setBatchResult(null);
    setTestResults({});
    try {
      setError(null);
      const outcomes = await Promise.all(
        providers.map(async (provider) => {
          try {
            const result = await testProvider(provider);
            setProviderTestResult(provider.id, {
              success: result.success,
              message: `连接${testMessage(result)}`,
            });
            return result.success;
          } catch (e) {
            setProviderTestResult(provider.id, { success: false, message: String(e) });
            return false;
          }
        }),
      );
      const passed = outcomes.filter(Boolean).length;

      setBatchResult({
        success: passed === providers.length,
        message: `服务商 ${passed}/${providers.length} 通过`,
      });
    } finally {
      setTesting(null);
    }
  };

  const addModel = () => {
    if (!editing || !modelInput.trim()) return;
    const nextModels = modelInput
      .split(",")
      .map((model) => model.trim())
      .filter(Boolean);
    setEditing({
      ...editing,
      models: Array.from(new Set([...editing.models, ...nextModels])),
    });
    setModelInput("");
  };

  const removeModel = (model: string) => {
    if (!editing) return;
    setEditing({
      ...editing,
      models: editing.models.filter((item) => item !== model),
    });
  };

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="metric-label">Provider Registry</div>
          <h1 className="mt-1 text-2xl font-semibold text-surface-950 dark:text-white">
            服务商
          </h1>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {batchResult && (
            <span
              className={`badge max-w-full truncate ${batchResult.success ? "badge-success" : "badge-error"}`}
              title={batchResult.message}
            >
              {batchResult.message}
            </span>
          )}
          <button
            className="btn-secondary"
            onClick={handleTestAllProviders}
            disabled={testing !== null || !config?.providers.length}
          >
            {testing === "all-providers" ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <ListChecks className="h-4 w-4" />
            )}
            {testing === "all-providers" ? "测试中" : "测试全部服务商"}
          </button>
          <button className="btn-primary" onClick={beginCreate}>
            <Plus className="h-4 w-4" />
            添加服务商
          </button>
        </div>
      </div>

      {error && (
        <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          {error}
        </div>
      )}

      {showForm && editing && (
        <section className="panel">
          <div className="flex items-center justify-between border-b border-surface-200 px-4 py-3 dark:border-surface-800">
            <div className="flex items-center gap-2">
              <Network className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
              <h2 className="text-sm font-semibold">
                {editing.id ? "编辑服务商" : "新建服务商"}
              </h2>
            </div>
            <button
              className="btn-icon"
              onClick={() => {
                setShowForm(false);
                setEditing(null);
              }}
              title="关闭"
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          <div className="grid grid-cols-1 gap-4 p-4 lg:grid-cols-4">
            <label className="space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">预设</span>
              <select
                className="input-field"
                defaultValue=""
                onChange={(e) => applyPreset(e.target.value)}
              >
                <option value="">自定义</option>
                {providerPresets.map((preset) => (
                  <option key={preset.id} value={preset.id}>
                    {preset.label}
                  </option>
                ))}
              </select>
            </label>
            <label className="space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">名称</span>
              <input
                className="input-field"
                placeholder="DeepSeek / Anthropic"
                value={editing.name}
                onChange={(e) => setEditing({ ...editing, name: e.target.value })}
              />
            </label>
            <label className="space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">协议</span>
              <select
                className="input-field"
                value={editing.protocol}
                onChange={(e) =>
                  setEditing({ ...editing, protocol: e.target.value as Provider["protocol"] })
                }
              >
                <option value="openai">OpenAI-compatible</option>
                <option value="anthropic">Anthropic Messages</option>
              </select>
            </label>
            <label className="space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">优先级</span>
              <input
                className="input-field"
                type="number"
                value={editing.priority}
                onChange={(e) =>
                  setEditing({ ...editing, priority: parseInt(e.target.value, 10) || 0 })
                }
              />
            </label>
            <label className="space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">状态</span>
              <button
                className="btn-secondary w-full"
                onClick={() => setEditing({ ...editing, enabled: !editing.enabled })}
              >
                {editing.enabled ? <CheckCircle2 className="h-4 w-4" /> : <CircleOff className="h-4 w-4" />}
                {editing.enabled ? "启用" : "禁用"}
              </button>
            </label>

            <label className="space-y-1.5 lg:col-span-2">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">基础地址</span>
              <input
                className="input-field"
                placeholder={
                  editing.protocol === "anthropic"
                    ? "https://api.anthropic.com 或 https://api.deepseek.com/anthropic"
                    : "https://api.openai.com 或 https://ark.cn-beijing.volces.com/api/v3"
                }
                value={editing.base_url}
                onChange={(e) => setEditing({ ...editing, base_url: e.target.value })}
              />
            </label>
            <label className="space-y-1.5 lg:col-span-2">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">API 密钥</span>
              <input
                className="input-field"
                type="password"
                placeholder={editing.protocol === "anthropic" ? "sk-ant-..." : "sk-..."}
                value={editing.api_key}
                onChange={(e) => setEditing({ ...editing, api_key: e.target.value })}
              />
            </label>

            <div className="space-y-2 lg:col-span-4">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">模型</span>
              <div className="flex gap-2">
                <input
                  className="input-field"
                  placeholder={
                    editing.protocol === "anthropic"
                      ? "claude-3-5-sonnet-latest, claude-opus-4-1"
                      : "deepseek-v4-flash, gpt-4o"
                  }
                  value={modelInput}
                  onChange={(e) => setModelInput(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && (e.preventDefault(), addModel())}
                />
                <button className="btn-secondary shrink-0" onClick={addModel}>
                  <Plus className="h-4 w-4" />
                  添加
                </button>
              </div>
              {editing.models.length > 0 && (
                <div className="flex flex-wrap gap-2">
                  {editing.models.map((model) => (
                    <button
                      key={model}
                      className="badge badge-neutral hover:bg-red-100 hover:text-red-700 dark:hover:bg-red-500/15 dark:hover:text-red-300"
                      onClick={() => removeModel(model)}
                    >
                      {model}
                      <X className="h-3 w-3" />
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="flex justify-end gap-2 border-t border-surface-200 px-4 py-3 dark:border-surface-800">
            <button
              className="btn-secondary"
              onClick={() => {
                setShowForm(false);
                setEditing(null);
              }}
            >
              取消
            </button>
            <button className="btn-primary" onClick={handleSave}>
              <Save className="h-4 w-4" />
              保存
            </button>
          </div>
        </section>
      )}

      <section className="space-y-2">
        {config?.providers.map((provider) => (
          <div key={provider.id} className="data-row p-4">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div className="flex min-w-0 items-center gap-3">
                <button
                  onClick={() => handleToggle(provider)}
                  className={`flex h-9 w-9 items-center justify-center rounded-lg ${
                    provider.enabled
                      ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300"
                      : "bg-surface-100 text-surface-500 dark:bg-surface-800 dark:text-surface-400"
                  }`}
                  title={provider.enabled ? "禁用" : "启用"}
                >
                  {provider.enabled ? <CheckCircle2 className="h-4 w-4" /> : <CircleOff className="h-4 w-4" />}
                </button>
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <h3 className="font-semibold text-surface-950 dark:text-white">
                      {provider.name || "未命名服务商"}
                    </h3>
                    <span className="badge badge-info">{protocolLabel(provider.protocol)}</span>
                    <span className="badge badge-neutral">P{provider.priority}</span>
                  </div>
                  <p className="mt-1 truncate font-mono text-xs text-surface-500 dark:text-surface-400">
                    {provider.base_url || "未配置基础地址"}
                  </p>
                </div>
              </div>

              <div className="flex items-center gap-2">
                {testResults[provider.id] && (
                  <span
                    className={`badge max-w-[22rem] truncate ${testResults[provider.id].success ? "badge-success" : "badge-error"}`}
                    title={testResults[provider.id].message}
                  >
                    {testResults[provider.id].message}
                  </span>
                )}
                <button
                  className="btn-secondary"
                  onClick={() => handleTest(provider)}
                  disabled={testing !== null}
                >
                  {testing === provider.id ? (
                    <Loader2 className="h-4 w-4 animate-spin" />
                  ) : (
                    <FlaskConical className="h-4 w-4" />
                  )}
                  {testing === provider.id ? "测试中" : "测试连接"}
                </button>
                <button
                  className="btn-secondary"
                  onClick={() => handleTestModels(provider)}
                  disabled={testing !== null}
                >
                  {testing === `${provider.id}:models` ? (
                    <Loader2 className="h-4 w-4 animate-spin" />
                  ) : (
                    <ListChecks className="h-4 w-4" />
                  )}
                  {testing === `${provider.id}:models` ? "测试中" : "全部模型"}
                </button>
                <button className="btn-icon" onClick={() => beginEdit(provider)} title="编辑">
                  <Pencil className="h-4 w-4" />
                </button>
                <button className="btn-icon" onClick={() => handleDelete(provider.id)} title="删除">
                  <Trash2 className="h-4 w-4" />
                </button>
              </div>
            </div>

            {provider.models.length > 0 && (
              <div className="mt-3 flex flex-wrap gap-2 pl-12">
                {provider.models.map((model) => (
                  <span key={model} className="badge badge-neutral">
                    {model}
                  </span>
                ))}
              </div>
            )}
          </div>
        ))}

        {(!config?.providers || config.providers.length === 0) && (
          <div className="panel flex min-h-64 flex-col items-center justify-center p-8 text-center">
            <Network className="mb-3 h-10 w-10 text-surface-300 dark:text-surface-700" />
            <p className="font-medium text-surface-800 dark:text-surface-200">暂无服务商</p>
            <button className="btn-primary mt-4" onClick={beginCreate}>
              <Plus className="h-4 w-4" />
              添加服务商
            </button>
          </div>
        )}
      </section>
    </div>
  );
}
