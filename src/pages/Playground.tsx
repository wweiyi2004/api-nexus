import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertCircle,
  Bot,
  Clipboard,
  Image as ImageIcon,
  Loader2,
  MessageSquare,
  Play,
  RefreshCw,
  SlidersHorizontal,
  Sparkles,
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
}

interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cached_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
}

interface PlaygroundImage {
  url: string | null;
  b64_json: string | null;
  mime_type: string | null;
  revised_prompt: string | null;
}

interface PlaygroundResponse {
  status: number;
  success: boolean;
  url: string;
  provider_id: string;
  provider_name: string;
  protocol: Provider["protocol"];
  model: string;
  content: string;
  images: PlaygroundImage[];
  usage: TokenUsage;
  raw_body: unknown;
  latency_ms: number;
}

interface PlaygroundModelOption {
  key: string;
  provider: Provider;
  model: string;
  label: string;
}

const defaultPrompt = "用三句话说明 API Nexus 的用途。";
const defaultImagePrompt = "一张 API Nexus 的产品海报，干净的桌面软件界面，科技感但克制";
type PlaygroundMode = "chat" | "image";

function modelKey(providerId: string, model: string) {
  return `${providerId}::${model}`;
}

function formatTokens(value: number) {
  return new Intl.NumberFormat("en-US").format(value);
}

function protocolLabel(protocol: Provider["protocol"]) {
  return protocol === "anthropic" ? "Anthropic" : "OpenAI";
}

function imageSource(image: PlaygroundImage) {
  if (image.url) return image.url;
  if (!image.b64_json) return "";
  return `data:${image.mime_type || "image/png"};base64,${image.b64_json}`;
}

export default function Playground() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [selectedKey, setSelectedKey] = useState("");
  const [mode, setMode] = useState<PlaygroundMode>("chat");
  const [systemPrompt, setSystemPrompt] = useState("You are concise and practical.");
  const [userPrompt, setUserPrompt] = useState(defaultPrompt);
  const [maxTokens, setMaxTokens] = useState(512);
  const [temperature, setTemperature] = useState(0.7);
  const [imageSize, setImageSize] = useState("1024x1024");
  const [imageCount, setImageCount] = useState(1);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<PlaygroundResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const fetchConfig = async () => {
    const appConfig = await invoke<AppConfig>("get_config");
    setConfig(appConfig);
  };

  useEffect(() => {
    fetchConfig().catch((e) => setError(String(e)));
  }, []);

  const modelOptions = useMemo<PlaygroundModelOption[]>(() => {
    const options: PlaygroundModelOption[] = [];
    for (const provider of config?.providers ?? []) {
      if (!provider.enabled) continue;
      for (const model of provider.models) {
        options.push({
          key: modelKey(provider.id, model),
          provider,
          model,
          label: `${provider.name || provider.id} · ${model}`,
        });
      }
    }
    return options;
  }, [config]);

  useEffect(() => {
    if (!selectedKey && modelOptions.length > 0) {
      setSelectedKey(modelOptions[0].key);
    }
  }, [modelOptions, selectedKey]);

  const selected = modelOptions.find((option) => option.key === selectedKey) ?? null;

  const run = async () => {
    if (!selected) {
      setError("请选择一个可用模型");
      return;
    }
    if (!userPrompt.trim()) {
      setError("请输入用户消息");
      return;
    }

    setRunning(true);
    setError(null);
    setCopied(false);
    try {
      const response = await invoke<PlaygroundResponse>("run_playground", {
        request: {
          provider_id: selected.provider.id,
          model: selected.model,
          mode,
          system_prompt: systemPrompt,
          user_prompt: userPrompt,
          max_tokens: maxTokens,
          temperature,
          image_size: imageSize,
          image_count: imageCount,
        },
      });
      setResult(response);
    } catch (e) {
      setError(String(e));
    } finally {
      setRunning(false);
    }
  };

  const copyResult = async () => {
    if (!result?.content) return;
    await navigator.clipboard.writeText(result.content);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="metric-label">Model Playground</div>
          <h1 className="mt-1 text-2xl font-semibold text-surface-950 dark:text-white">
            Playground
          </h1>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <button
            className="btn-secondary"
            onClick={() => fetchConfig().catch((e) => setError(String(e)))}
            disabled={running}
          >
            <RefreshCw className="h-4 w-4" />
            刷新
          </button>
          <button
            className="btn-primary"
            onClick={run}
            disabled={running || !selected || !userPrompt.trim()}
          >
            {running ? <Loader2 className="h-4 w-4 animate-spin" /> : <Play className="h-4 w-4" />}
            {running ? "运行中" : "运行"}
          </button>
        </div>
      </div>

      {error && (
        <div className="flex items-start gap-2 rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
          <AlertCircle className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      <section className="grid grid-cols-1 gap-4 xl:grid-cols-[22rem_minmax(0,1fr)]">
        <aside className="space-y-4">
          <section className="panel p-4">
            <div className="mb-3 flex items-center gap-2">
              <Bot className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
              <h2 className="text-sm font-semibold">模型</h2>
            </div>
            <label className="space-y-1.5">
              <span className="text-sm font-medium text-surface-700 dark:text-surface-300">
                路由
              </span>
              <select
                className="input-field"
                value={selectedKey}
                onChange={(event) => setSelectedKey(event.target.value)}
              >
                {modelOptions.map((option) => (
                  <option key={option.key} value={option.key}>
                    {option.label}
                  </option>
                ))}
              </select>
            </label>
            {selected ? (
              <div className="mt-3 space-y-2 rounded-lg border border-surface-200 bg-surface-50 p-3 text-xs dark:border-surface-800 dark:bg-surface-950">
                <div className="flex items-center justify-between gap-3">
                  <span className="text-surface-500 dark:text-surface-400">协议</span>
                  <span className="badge badge-info">{protocolLabel(selected.provider.protocol)}</span>
                </div>
                <div className="flex items-center justify-between gap-3">
                  <span className="text-surface-500 dark:text-surface-400">服务商</span>
                  <span className="truncate text-right font-medium text-surface-700 dark:text-surface-200">
                    {selected.provider.name || selected.provider.id}
                  </span>
                </div>
                <div className="break-all font-mono text-[11px] text-surface-500 dark:text-surface-400">
                  {selected.provider.base_url}
                </div>
              </div>
            ) : (
              <div className="mt-3 rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
                先在“服务商”里添加并启用至少一个模型。
              </div>
            )}
          </section>

          <section className="panel p-4">
            <div className="mb-3 flex items-center gap-2">
              <SlidersHorizontal className="h-4 w-4 text-emerald-600 dark:text-emerald-300" />
              <h2 className="text-sm font-semibold">参数</h2>
            </div>
            <div className="space-y-4">
              <div className="grid grid-cols-2 gap-2">
                <button
                  className={mode === "chat" ? "btn-primary" : "btn-secondary"}
                  onClick={() => {
                    setMode("chat");
                    if (userPrompt === defaultImagePrompt) setUserPrompt(defaultPrompt);
                  }}
                >
                  <MessageSquare className="h-4 w-4" />
                  对话
                </button>
                <button
                  className={mode === "image" ? "btn-primary" : "btn-secondary"}
                  onClick={() => {
                    setMode("image");
                    if (userPrompt === defaultPrompt) setUserPrompt(defaultImagePrompt);
                  }}
                >
                  <ImageIcon className="h-4 w-4" />
                  生图
                </button>
              </div>

              {mode === "chat" ? (
                <>
                  <label className="space-y-1.5">
                    <span className="text-sm font-medium text-surface-700 dark:text-surface-300">
                      Max tokens
                    </span>
                    <input
                      type="number"
                      min={1}
                      max={128000}
                      className="input-field"
                      value={maxTokens}
                      onChange={(event) => setMaxTokens(Number(event.target.value))}
                    />
                  </label>
                  <label className="space-y-1.5">
                    <span className="flex items-center justify-between text-sm font-medium text-surface-700 dark:text-surface-300">
                      Temperature
                      <span className="font-mono text-xs text-surface-500">{temperature.toFixed(1)}</span>
                    </span>
                    <input
                      type="range"
                      min={0}
                      max={2}
                      step={0.1}
                      value={temperature}
                      onChange={(event) => setTemperature(Number(event.target.value))}
                      className="w-full accent-cyan-600"
                    />
                  </label>
                </>
              ) : (
                <>
                  <label className="space-y-1.5">
                    <span className="text-sm font-medium text-surface-700 dark:text-surface-300">
                      Size
                    </span>
                    <select
                      className="input-field"
                      value={imageSize}
                      onChange={(event) => setImageSize(event.target.value)}
                    >
                      <option value="1024x1024">1024x1024</option>
                      <option value="1024x1536">1024x1536</option>
                      <option value="1536x1024">1536x1024</option>
                      <option value="512x512">512x512</option>
                    </select>
                  </label>
                  <label className="space-y-1.5">
                    <span className="text-sm font-medium text-surface-700 dark:text-surface-300">
                      Images
                    </span>
                    <input
                      type="number"
                      min={1}
                      max={4}
                      className="input-field"
                      value={imageCount}
                      onChange={(event) => setImageCount(Number(event.target.value))}
                    />
                  </label>
                  {selected?.provider.protocol === "anthropic" && (
                    <div className="rounded-lg border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
                      生图需要 OpenAI-compatible 服务商。
                    </div>
                  )}
                </>
              )}
            </div>
          </section>
        </aside>

        <div className="space-y-4">
          <section className="panel p-4">
            <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
              <label className="space-y-1.5 lg:col-span-2">
                <span className="text-sm font-medium text-surface-700 dark:text-surface-300">
                  System
                </span>
                <textarea
                  className="input-field min-h-24 resize-y"
                  value={systemPrompt}
                  onChange={(event) => setSystemPrompt(event.target.value)}
                  disabled={mode === "image"}
                />
              </label>
              <label className="space-y-1.5 lg:col-span-2">
                <span className="text-sm font-medium text-surface-700 dark:text-surface-300">
                  {mode === "image" ? "Prompt" : "User"}
                </span>
                <textarea
                  className="input-field min-h-44 resize-y"
                  value={userPrompt}
                  onChange={(event) => setUserPrompt(event.target.value)}
                  onKeyDown={(event) => {
                    if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
                      event.preventDefault();
                      void run();
                    }
                  }}
                />
              </label>
            </div>
          </section>

          <section className="panel overflow-hidden">
            <div className="flex flex-wrap items-center justify-between gap-3 border-b border-surface-200 px-4 py-3 dark:border-surface-800">
              <div className="flex items-center gap-2">
                <Sparkles className="h-4 w-4 text-cyan-600 dark:text-cyan-300" />
                <h2 className="text-sm font-semibold">结果</h2>
              </div>
              <div className="flex items-center gap-2">
                {result && (
                  <>
                    <span className="badge badge-success">HTTP {result.status}</span>
                    <span className="badge badge-neutral">{result.latency_ms}ms</span>
                  </>
                )}
                <button
                  className="btn-secondary"
                  onClick={copyResult}
                  disabled={!result?.content || mode === "image"}
                >
                  <Clipboard className="h-4 w-4" />
                  {copied ? "已复制" : "复制"}
                </button>
              </div>
            </div>

            {result ? (
              <div className="space-y-4 p-4">
                <div className="grid grid-cols-2 gap-2 text-sm md:grid-cols-4">
                  <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                    <div className="metric-label">Input</div>
                    <div className="mt-1 font-semibold">{formatTokens(result.usage.input_tokens)}</div>
                  </div>
                  <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                    <div className="metric-label">Output</div>
                    <div className="mt-1 font-semibold">{formatTokens(result.usage.output_tokens)}</div>
                  </div>
                  <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                    <div className="metric-label">Cache Read</div>
                    <div className="mt-1 font-semibold">{formatTokens(result.usage.cache_read_tokens)}</div>
                  </div>
                  <div className="rounded-lg bg-surface-50 px-3 py-2 dark:bg-surface-950">
                    <div className="metric-label">Provider</div>
                    <div className="mt-1 truncate font-semibold">{result.provider_name || result.provider_id}</div>
                  </div>
                </div>
                {result.images.length > 0 ? (
                  <div
                    data-testid="playground-image-results"
                    className="grid grid-cols-1 gap-3 sm:grid-cols-2"
                  >
                    {result.images.map((image, index) => {
                      const src = imageSource(image);
                      return (
                        <figure
                          key={`${src.slice(0, 48)}-${index}`}
                          className="overflow-hidden rounded-lg border border-surface-200 bg-surface-50 dark:border-surface-800 dark:bg-surface-950"
                        >
                          <img
                            src={src}
                            alt={image.revised_prompt || userPrompt}
                            className="aspect-square w-full object-contain bg-white dark:bg-surface-900"
                          />
                          <figcaption className="flex flex-wrap items-center justify-between gap-2 px-3 py-2 text-xs text-surface-500 dark:text-surface-400">
                            <span className="truncate">{image.revised_prompt || `Image ${index + 1}`}</span>
                            <a
                              className="font-medium text-cyan-700 hover:text-cyan-600 dark:text-cyan-300"
                              href={src}
                              target="_blank"
                              rel="noreferrer"
                              download={image.url ? undefined : `api-nexus-image-${index + 1}.png`}
                            >
                              打开
                            </a>
                          </figcaption>
                        </figure>
                      );
                    })}
                  </div>
                ) : (
                  <div
                    data-testid="playground-result-content"
                    className="whitespace-pre-wrap rounded-lg border border-surface-200 bg-surface-50 p-4 text-sm leading-6 text-surface-800 dark:border-surface-800 dark:bg-surface-950 dark:text-surface-100"
                  >
                    {result.content || (mode === "image" ? "模型没有返回图片。" : "模型没有返回文本内容。")}
                  </div>
                )}
                <details className="rounded-lg border border-surface-200 dark:border-surface-800">
                  <summary className="cursor-pointer px-3 py-2 text-sm font-medium">
                    原始响应 JSON
                  </summary>
                  <pre className="max-h-96 overflow-auto border-t border-surface-200 bg-surface-50 p-3 text-xs leading-5 dark:border-surface-800 dark:bg-surface-950">
                    {JSON.stringify(result.raw_body, null, 2)}
                  </pre>
                </details>
              </div>
            ) : (
              <div className="flex min-h-72 items-center justify-center p-8 text-sm text-surface-500 dark:text-surface-400">
                选择模型并运行后会显示响应。
              </div>
            )}
          </section>
        </div>
      </section>
    </div>
  );
}
