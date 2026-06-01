import { Outlet } from "react-router-dom";
import Sidebar from "./Sidebar";

export default function Layout() {
  return (
    <div className="app-shell flex min-h-screen flex-col md:h-screen md:flex-row md:overflow-hidden">
      <Sidebar />
      <main className="min-h-0 flex-1 overflow-auto">
        <div className="mx-auto w-full max-w-7xl px-4 py-4 sm:px-6 sm:py-5">
          <Outlet />
        </div>
      </main>
    </div>
  );
}
