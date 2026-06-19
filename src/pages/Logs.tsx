import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Activity, ArrowDown, ArrowUp, RefreshCw, ScrollText, Trash2 } from "lucide-react";

interface RequestLogEntry {
  timestamp: number;
  method: string;
  path: string;
  model: string;
  provider: string;
  status: number;
  input_tokens: number;
  output_tokens: number;
  duration_ms: number;
  error: string | null;
}

function formatTime(ts: number) {
  const d = new Date(ts * 1000);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function statusClass(status: number) {
  if (status >= 200 && status < 300) return "badge-success";
  if (status >= 400 && status < 500) return "badge-warning";
  return "badge-error";
}

export default function Logs() {
  const [logs, setLogs] = useState<RequestLogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [autoRefresh, setAutoRefresh] = useState(true);

  const fetchLogs = async () => {
    try {
      const entries = await invoke<RequestLogEntry[]>("get_request_logs");
      setLogs(entries);
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
            发起 API 请求后，最近 100 条记录会显示在这里
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
                  <th className="px-3 py-2 font-medium">状态</th>
                  <th className="px-3 py-2 text-right font-medium">耗时</th>
                  <th className="px-3 py-2 text-right font-medium">In</th>
                  <th className="px-3 py-2 text-right font-medium">Out</th>
                  <th className="px-3 py-2 font-medium">错误</th>
                </tr>
              </thead>
              <tbody>
                {logs.map((log, index) => (
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
                    <td className="max-w-[20rem] truncate px-3 py-2 text-xs text-red-600 dark:text-red-400" title={log.error ?? ""}>
                      {log.error ?? ""}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}
    </div>
  );
}
