import {
  Boxes,
  BrainCircuit,
  Gauge,
  Moon,
  Network,
  ScrollText,
  Settings,
  Sun,
  Waypoints,
} from "lucide-react";
import { NavLink } from "react-router-dom";
import { useTheme } from "../contexts/ThemeContext";

const navItems = [
  { to: "/", label: "控制台", icon: Gauge, end: true },
  { to: "/providers", label: "服务商", icon: Network },
  { to: "/models", label: "模型路由", icon: Boxes },
  { to: "/fusion", label: "Fusion", icon: BrainCircuit },
  { to: "/logs", label: "请求日志", icon: ScrollText },
  { to: "/settings", label: "设置", icon: Settings },
];

export default function Sidebar() {
  const { theme, toggleTheme } = useTheme();
  const ThemeIcon = theme === "dark" ? Sun : Moon;

  return (
    <aside className="sidebar flex w-full shrink-0 flex-col md:h-screen md:w-64">
      <div className="flex h-16 items-center gap-3 border-b border-surface-200 px-4 dark:border-surface-800">
        <img
          src="/app-icon.png"
          alt=""
          className="h-9 w-9 rounded-lg border border-surface-200 dark:border-surface-700"
        />
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold text-surface-950 dark:text-white">
            API Nexus
          </div>
          <div className="truncate text-xs text-surface-500 dark:text-surface-400">
            Local Gateway
          </div>
        </div>
      </div>

      <div className="px-3 py-3">
        <div className="mb-2 hidden items-center gap-2 px-2 text-[11px] font-semibold uppercase tracking-wide text-surface-400 md:flex">
          <Waypoints className="h-3.5 w-3.5" />
          Navigation
        </div>
        <nav className="flex gap-2 overflow-x-auto pb-1 md:block md:space-y-1 md:overflow-visible md:pb-0">
          {navItems.map((item) => {
            const Icon = item.icon;
            return (
              <NavLink
                key={item.to}
                to={item.to}
                end={item.end}
                className={({ isActive }) =>
                  `flex shrink-0 items-center gap-2 rounded-lg px-3 py-2 text-sm font-medium transition-colors md:w-full md:gap-3 ${
                    isActive
                      ? "bg-surface-900 text-white dark:bg-cyan-500 dark:text-surface-950"
                      : "text-surface-600 hover:bg-surface-100 hover:text-surface-950 dark:text-surface-300 dark:hover:bg-surface-800 dark:hover:text-white"
                  }`
                }
              >
                <Icon className="h-4 w-4 shrink-0" />
                <span className="truncate">{item.label}</span>
              </NavLink>
            );
          })}
        </nav>
      </div>

      <div className="border-t border-surface-200 p-3 dark:border-surface-800 md:mt-auto">
        <button
          onClick={toggleTheme}
          className="flex w-full items-center justify-center gap-2 rounded-lg px-3 py-2 text-sm font-medium text-surface-600 transition-colors hover:bg-surface-100 hover:text-surface-950 dark:text-surface-300 dark:hover:bg-surface-800 dark:hover:text-white md:justify-start md:gap-3"
        >
          <ThemeIcon className="h-4 w-4" />
          {theme === "dark" ? "浅色模式" : "深色模式"}
        </button>
      </div>
    </aside>
  );
}
