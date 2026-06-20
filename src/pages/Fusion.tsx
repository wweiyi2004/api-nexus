import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useLocation } from "react-router-dom";
import {
  BrainCircuit,
  ChevronDown,
  GitMerge,
  Loader2,
  Play,
  Plus,
  RefreshCw,
  Save,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";

interface ModelRef {
  provider_id: string;
  model: string;
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

interface FusionConfig {
  enabled: boolean;
  panel_models: ModelRef[];
  judge_model: ModelRef | null;
  final_model: ModelRef | null;
  max_panel_models: number;
  timeout_secs: number;
}

interface AppConfig {
  providers: Provider[];
  fusion: FusionConfig;
}

interface FusionRunEntry {
  id: number;
  created_at: number;
  source_log_id: number | null;
  input_protocol: string;
  status: string;
  duration_ms: number;
  panel_count: number;
  total_tokens: number;
  estimated_cost: number;
  final_content: string | null;
  error: string | null;
}

interface FusionStepEntry {
  id: number;
  run_id: number;
  role: "panel" | "judge" | "final" | string;
  provider_id: string;
  model: string;
  status: string;
  latency_ms: number;
  prompt_tokens: number;
  completion_tokens: number;
  cost: number;
  content: string | null;
  error: string | null;
}

interface FusionRunDetails {
  run: FusionRunEntry;
  steps: FusionStepEntry[];
}

interface ReplayState {
  requestBody?: string | null;
  sourceLogId?: number;
  path?: string;
}

const emptyModelRef: ModelRef = { provider_id: "", model: "" };

function modelRefKey(modelRef: ModelRef | null | undefined) {
  return modelRef?.provider_id && modelRef.model
    ? `${modelRef.provider_id}::${modelRef.model}`
    : "";
}

function modelRefFromKey(key: string): ModelRef {
  const [provider_id, ...modelParts] = key.split("::");
  return { provider_id: provider_id ?? "", model: modelParts.join("::") };
}

function sameModelRef(a: ModelRef, b: ModelRef) {
  return a.provider_id === b.provider_id && a.model === b.model;
}

function compactModelRef(modelRef: ModelRef) {
  return `${modelRef.provider_id} / ${modelRef.model}`;
}

function formatTime(ts: number) {
  const d = new Date(ts * 1000);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function formatTokens(value: number) {
  return new Intl.NumberFormat("en-US").format(value);
}

function formatMoney(value: number) {
  if (value === 0) return "$0.0000";
  return `$${value < 0.01 ? value.toFixed(6) : value.toFixed(4)}`;
}

function contentText(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((item) => {
        if (!item || typeof item !== "object") return "";
        const record = item as Record<string, unknown>;
        if (record.type === "text" && typeof record.text === "string") return record.text;
        return "";
      })
      .filter(Boolean)
      .join("\n");
  }
  return "";
}

function requestMessagesFromReplay(path: string | undefined, requestBody: string) {
  const parsed = JSON.parse(requestBody) as Record<string, unknown>;
  if (path === "/v1/messages") {
    const messages: Array<Record<string, unknown>> = [];
    const system = contentText(parsed.system);
    if (system) messages.push({ role: "system", content: system });
    for (const message of Array.isArray(parsed.messages) ? parsed.messages : []) {
      if (!message || typeof message !== "object") continue;
      const record = message as Record<string, unknown>;
      const role = typeof record.role === "string" ? record.role : "user";
      messages.push({ role, content: contentText(record.content) });
    }
    return messages;
  }

  const messages = Array.isArray(parsed.messages) ? parsed.messages : [];
  return messages.filter((message) => message && typeof message === "object");
}

function lastUserPrompt(messages: unknown[]) {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index] as Record<string, unknown>;
    if (message.role === "user") return contentText(message.content);
  }
  return "";
}

function roleLabel(role: string) {
  if (role === "panel") return "Panel";
  if (role === "judge") return "Judge";
  if (role === "final") return "Final";
  return role;
}

function statusBadge(status: string) {
  if (status === "succeeded") return "badge-success";
  if (status === "running") return "badge-info";
  return "badge-error";
}

export default function Fusion() {
  const location = useLocation();
  const replayApplied = useRef(false);
  const defaultsApplied = useRef(false);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [runs, setRuns] = useState<FusionRunEntry[]>([]);
  const [details, setDetails] = useState<FusionRunDetails | null>(null);
  const [prompt, setPrompt] = useState("");
  const [messagesJson, setMessagesJson] = useState("");
  const [inputProtocol, setInputProtocol] = useState<"openai" | "anthropic">("openai");
  const [sourceLogId, setSourceLogId] = useState<number | null>(null);
  const [panelModels, setPanelModels] = useState<ModelRef[]>([]);
  const [judgeModel, setJudgeModel] = useState<ModelRef>(emptyModelRef);
  const [finalModel, setFinalModel] = useState<ModelRef>(emptyModelRef);
  const [running, setRunning] = useState(false);
  const [savingDefaults, setSavingDefaults] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [expandedSteps, setExpandedSteps] = useState<Record<number, boolean>>({});

  const fetchData = async () => {
    const [appConfig, fusionRuns] = await Promise.all([
      invoke<AppConfig>("get_config"),
      invoke<FusionRunEntry[]>("get_fusion_runs"),
    ]);
    setConfig(appConfig);
    setRuns(fusionRuns);
  };

  useEffect(() => {
    fetchData().catch((e) => setError(String(e)));
  }, []);

  const modelOptions = useMemo(() => {
    const options: Array<{ key: string; label: string; ref: ModelRef }> = [];
    for (const provider of config?.providers ?? []) {
      if (!provider.enabled) continue;
      for (const model of provider.models) {
        const ref = { provider_id: provider.id, model };
        options.push({
          key: modelRefKey(ref),
          label: `${provider.name || provider.id} · ${model}`,
          ref,
        });
      }
    }
    return options;
  }, [config]);

  useEffect(() => {
    if (!config || modelOptions.length === 0) return;
    if (!defaultsApplied.current) {
      const defaults = config.fusion.panel_models.length > 0
        ? config.fusion.panel_models
        : modelOptions.slice(0, Math.min(2, modelOptions.length)).map((item) => item.ref);
      setPanelModels(defaults);
      setJudgeModel(config.fusion.judge_model ?? modelOptions[0].ref);
      setFinalModel(config.fusion.final_model ?? config.fusion.judge_model ?? modelOptions[0].ref);
      defaultsApplied.current = true;
    }
  }, [config, modelOptions]);

  useEffect(() => {
    if (replayApplied.current) return;
    const replay = location.state as ReplayState | null;
    if (!replay?.requestBody) return;
    replayApplied.current = true;
    try {
      const messages = requestMessagesFromReplay(replay.path, replay.requestBody);
      setMessagesJson(JSON.stringify(messages, null, 2));
      setPrompt(lastUserPrompt(messages));
      setInputProtocol(replay.path === "/v1/messages" ? "anthropic" : "openai");
      setSourceLogId(replay.sourceLogId ?? null);
      setNotice(replay.sourceLogId ? `已载入请求日志 #${replay.sourceLogId}` : "已载入请求日志");
    } catch (e) {
      setError(`无法解析请求日志内容：${String(e)}`);
    }
  }, [location.state]);

  const loadRun = async (id: number) => {
    try {
      setError(null);
      const run = await invoke<FusionRunDetails>("get_fusion_run", { id });
      setDetails(run);
    } catch (e) {
      setError(String(e));
    }
  };

  const addPanelModel = () => {
    const next = modelOptions.find((item) => !panelModels.some((model) => sameModelRef(model, item.ref)));
    if (next) setPanelModels([...panelModels, next.ref]);
  };

  const updatePanelModel = (index: number, key: string) => {
    const next = [...panelModels];
    next[index] = modelRefFromKey(key);
    setPanelModels(next);
  };

  const removePanelModel = (index: number) => {
    setPanelModels(panelModels.filter((_, itemIndex) => itemIndex !== index));
  };

  const parseMessages = () => {
    if (!messagesJson.trim()) return [];
    const parsed = JSON.parse(messagesJson);
    if (!Array.isArray(parsed)) throw new Error("messages JSON must be an array");
    return parsed;
  };

  const runFusion = async () => {
    try {
      setError(null);
      setNotice(null);
      if (panelModels.length === 0) throw new Error("请选择 panel 模型");
      if (!judgeModel.provider_id || !judgeModel.model) throw new Error("请选择 judge 模型");
      setRunning(true);
      const run = await invoke<FusionRunDetails>("run_fusion", {
        request: {
          input_protocol: inputProtocol,
          prompt,
          messages: parseMessages(),
          source_log_id: sourceLogId,
          nexus_fusion: {
            panel_models: panelModels,
            judge_model: judgeModel,
            final_model: finalModel.provider_id ? finalModel : null,
          },
        },
      });
      setDetails(run);
      await fetchData();
    } catch (e) {
      setError(String(e));
      await fetchData().catch(() => undefined);
    } finally {
      setRunning(false);
    }
  };

  const saveDefaults = async () => {
    if (!config) return;
    try {
      setError(null);
      setSavingDefaults(true);
      const nextConfig = {
        ...config,
        fusion: {
          ...config.fusion,
          enabled: true,
          panel_models: panelModels,
          judge_model: judgeModel,
          final_model: finalModel.provider_id ? finalModel : null,
        },
      };
      const saved = await invoke<AppConfig>("save_config_cmd", { config: nextConfig });
      setConfig(saved);
      setNotice("Fusion 默认模型已保存");
    } catch (e) {
      setError(String(e));
    } finally {
      setSavingDefaults(false);
    }
  };

  const clearRuns = async () => {
    try {
      setError(null);
      await invoke("clear_fusion_runs");
      setRuns([]);
      setDetails(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const selectedRun = details?.run;

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="metric-label">Fusion Workbench</div>
          <h1 className="mt-1 text-2xl font-semibold text-surface-950 dark:text-white">
            Fusion
          </h1>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button className="btn-secondary" onClick={() => fetchData().catch((e) => setError(String(e)))}>
            <RefreshCw className="h-4 w-4" />
            刷新
          </button>
          <button className="btn-secondary" onClick={saveDefaults} disabled={savingDefaults || panelModels.length === 0 || !judgeModel.provider_id}>
            {savingDefaults ? <Loader2 className="h-4 w-4 animate-spin" /> : <Save className="h-4 w-4" />}
            保存默认
          </button>
          <button className="btn-primary" onClick={runFusion} disabled={running || panelModels.length === 0 || !judgeModel.provider_id}>
            {running ? <Loader2 className="h-4 w-4 animate-spin" /> : <Play className="h-4 w-4" />}
            {running ? "运行中" : "运行"}
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

      <section className="grid grid-cols-1 gap-4 xl:grid-cols-[minmax(0,1fr)_22rem]">
        <div className="space-y-4">
          <section className="panel p-4">
            <div className="mb-3 flex items-center gap-2">
              <BrainCircuit className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
              <h2 className="text-sm font-semibold">输入</h2>
            </div>
            <div className="grid grid-cols-1 gap-3 lg:grid-cols-[12rem_1fr]">
              <label className="space-y-1.5">
                <span className="text-sm font-medium text-surface-700 dark:text-surface-300">协议</span>
                <select
                  className="input-field"
                  value={inputProtocol}
                  onChange={(event) => setInputProtocol(event.target.value as "openai" | "anthropic")}
                >
                  <option value="openai">OpenAI</option>
                  <option value="anthropic">Anthropic</option>
                </select>
              </label>
              <label className="space-y-1.5">
                <span className="text-sm font-medium text-surface-700 dark:text-surface-300">来源日志</span>
                <input
                  className="input-field"
                  value={sourceLogId ?? ""}
                  placeholder="无"
                  onChange={(event) => {
                    const value = event.target.value.trim();
                    const parsed = Number(value);
                    setSourceLogId(value && Number.isFinite(parsed) ? parsed : null);
                  }}
                />
              </label>
            </div>
            <label className="mt-3 block space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">Prompt</span>
              <textarea
                className="input-field min-h-32 resize-y"
                value={prompt}
                onChange={(event) => setPrompt(event.target.value)}
              />
            </label>
            <label className="mt-3 block space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">Messages JSON</span>
              <textarea
                className="input-field min-h-44 resize-y font-mono text-xs"
                value={messagesJson}
                onChange={(event) => setMessagesJson(event.target.value)}
              />
            </label>
          </section>

          <section className="panel p-4">
            <div className="mb-3 flex flex-wrap items-center justify-between gap-3">
              <div className="flex items-center gap-2">
                <GitMerge className="h-4 w-4 text-emerald-600 dark:text-emerald-300" />
                <h2 className="text-sm font-semibold">模型</h2>
              </div>
              <button
                className="btn-secondary"
                onClick={addPanelModel}
                disabled={panelModels.length >= (config?.fusion.max_panel_models ?? 8) || modelOptions.length === 0}
              >
                <Plus className="h-4 w-4" />
                Panel
              </button>
            </div>

            <div className="space-y-2">
              {panelModels.map((modelRef, index) => (
                <div key={`${index}-${modelRefKey(modelRef)}`} className="grid grid-cols-[4.5rem_1fr_auto] items-center gap-2">
                  <span className="badge badge-neutral justify-center">Panel {index + 1}</span>
                  <select
                    className="input-field"
                    value={modelRefKey(modelRef)}
                    onChange={(event) => updatePanelModel(index, event.target.value)}
                  >
                    <option value="">选择模型</option>
                    {modelOptions.map((option) => (
                      <option key={option.key} value={option.key}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                  <button className="btn-icon" title="移除" onClick={() => removePanelModel(index)}>
                    <X className="h-4 w-4" />
                  </button>
                </div>
              ))}
            </div>

            <div className="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2">
              <label className="space-y-1.5">
                <span className="text-sm font-medium text-surface-700 dark:text-surface-300">Judge</span>
                <select className="input-field" value={modelRefKey(judgeModel)} onChange={(event) => setJudgeModel(modelRefFromKey(event.target.value))}>
                  <option value="">选择模型</option>
                  {modelOptions.map((option) => (
                    <option key={option.key} value={option.key}>{option.label}</option>
                  ))}
                </select>
              </label>
              <label className="space-y-1.5">
                <span className="text-sm font-medium text-surface-700 dark:text-surface-300">Final</span>
                <select className="input-field" value={modelRefKey(finalModel)} onChange={(event) => setFinalModel(modelRefFromKey(event.target.value))}>
                  <option value="">复用 Judge</option>
                  {modelOptions.map((option) => (
                    <option key={option.key} value={option.key}>{option.label}</option>
                  ))}
                </select>
              </label>
            </div>
          </section>

          <section className="panel overflow-hidden">
            <div className="flex items-center justify-between border-b border-surface-200 px-4 py-3 dark:border-surface-800">
              <div className="flex items-center gap-2">
                <Sparkles className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
                <h2 className="text-sm font-semibold">结果</h2>
              </div>
              {selectedRun && <span className={`badge ${statusBadge(selectedRun.status)}`}>{selectedRun.status}</span>}
            </div>
            {selectedRun ? (
              <div className="space-y-4 p-4">
                <div className="grid grid-cols-2 gap-2 text-sm md:grid-cols-4">
                  <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                    <div className="metric-label">Panels</div>
                    <div className="mt-1 font-semibold">{selectedRun.panel_count}</div>
                  </div>
                  <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                    <div className="metric-label">Tokens</div>
                    <div className="mt-1 font-semibold">{formatTokens(selectedRun.total_tokens)}</div>
                  </div>
                  <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                    <div className="metric-label">Cost</div>
                    <div className="mt-1 font-semibold">{formatMoney(selectedRun.estimated_cost)}</div>
                  </div>
                  <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                    <div className="metric-label">Latency</div>
                    <div className="mt-1 font-semibold">{selectedRun.duration_ms}ms</div>
                  </div>
                </div>
                {selectedRun.final_content && (
                  <div className="whitespace-pre-wrap rounded-lg border border-surface-200 bg-surface-50 p-3 text-sm leading-6 text-surface-800 dark:border-surface-800 dark:bg-surface-950 dark:text-surface-100">
                    {selectedRun.final_content}
                  </div>
                )}
                {selectedRun.error && (
                  <div className="rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
                    {selectedRun.error}
                  </div>
                )}
                <div className="space-y-2">
                  {details.steps.map((step) => {
                    const expanded = expandedSteps[step.id] ?? step.role !== "panel";
                    return (
                      <article key={step.id} className="rounded-lg border border-surface-200 dark:border-surface-800">
                        <button
                          className="flex w-full items-center gap-2 px-3 py-2 text-left text-sm"
                          onClick={() => setExpandedSteps((current) => ({ ...current, [step.id]: !expanded }))}
                        >
                          <span className={`badge ${statusBadge(step.status)}`}>{roleLabel(step.role)}</span>
                          <span className="min-w-0 flex-1 truncate font-medium">{compactModelRef({ provider_id: step.provider_id, model: step.model })}</span>
                          <span className="font-mono text-xs text-surface-500">{step.latency_ms}ms</span>
                          <ChevronDown className={`h-4 w-4 shrink-0 text-surface-400 transition-transform ${expanded ? "rotate-180" : ""}`} />
                        </button>
                        {expanded && (
                          <div className="border-t border-surface-100 bg-surface-50 p-3 text-sm dark:border-surface-800 dark:bg-surface-950">
                            <div className="mb-2 flex flex-wrap gap-2 text-xs">
                              <span className="badge badge-neutral">In {formatTokens(step.prompt_tokens)}</span>
                              <span className="badge badge-neutral">Out {formatTokens(step.completion_tokens)}</span>
                              <span className="badge badge-neutral">{formatMoney(step.cost)}</span>
                            </div>
                            {step.error ? (
                              <div className="text-red-600 dark:text-red-300">{step.error}</div>
                            ) : (
                              <pre className="whitespace-pre-wrap font-sans leading-6 text-surface-700 dark:text-surface-200">{step.content}</pre>
                            )}
                          </div>
                        )}
                      </article>
                    );
                  })}
                </div>
              </div>
            ) : (
              <div className="flex min-h-56 items-center justify-center p-6 text-sm text-surface-500 dark:text-surface-400">
                暂无结果
              </div>
            )}
          </section>
        </div>

        <aside className="panel h-fit overflow-hidden">
          <div className="flex items-center justify-between border-b border-surface-200 px-4 py-3 dark:border-surface-800">
            <h2 className="text-sm font-semibold">运行记录</h2>
            <button className="btn-icon" title="清空" onClick={clearRuns} disabled={runs.length === 0}>
              <Trash2 className="h-4 w-4" />
            </button>
          </div>
          <div className="max-h-[42rem] divide-y divide-surface-100 overflow-y-auto dark:divide-surface-800">
            {runs.map((run) => (
              <button
                key={run.id}
                className={`w-full px-4 py-3 text-left text-sm transition-colors hover:bg-surface-50 dark:hover:bg-surface-950 ${
                  details?.run.id === run.id ? "bg-surface-50 dark:bg-surface-950" : ""
                }`}
                onClick={() => loadRun(run.id)}
              >
                <div className="flex items-center justify-between gap-3">
                  <span className="font-semibold">#{run.id}</span>
                  <span className={`badge ${statusBadge(run.status)}`}>{run.status}</span>
                </div>
                <div className="mt-1 font-mono text-[11px] text-surface-500 dark:text-surface-400">
                  {formatTime(run.created_at)}
                </div>
                <div className="mt-2 flex flex-wrap gap-2 text-xs text-surface-500 dark:text-surface-400">
                  <span>{run.panel_count} panels</span>
                  <span>{formatTokens(run.total_tokens)} tokens</span>
                  <span>{formatMoney(run.estimated_cost)}</span>
                </div>
                {run.final_content && (
                  <div className="mt-2 line-clamp-2 text-xs text-surface-600 dark:text-surface-300">
                    {run.final_content}
                  </div>
                )}
              </button>
            ))}
            {runs.length === 0 && (
              <div className="px-4 py-8 text-center text-sm text-surface-500 dark:text-surface-400">
                暂无运行记录
              </div>
            )}
          </div>
        </aside>
      </section>
    </div>
  );
}
