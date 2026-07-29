"use client";

import { usePathname } from "next/navigation";

// 侧边栏：面板导航（宝塔风格）。M1 仅"工作台"可用，其余为占位。
const ITEMS = [
  { href: "/", label: "工作台" },
  { href: "/sessions", label: "会话" },
  { href: "/agents", label: "智能体" },
  { href: "/settings", label: "设置" },
];

export default function Sidebar() {
  const pathname = usePathname();
  return (
    <aside className="sidebar">
      <div className="logo">agentOS</div>
      <nav>
        {ITEMS.map((it) => {
          const active =
            it.href === "/"
              ? pathname === "/"
              : pathname.startsWith(it.href);
          return (
            <a
              key={it.href}
              className={active ? "nav-item active" : "nav-item"}
              href={it.href}
            >
              {it.label}
            </a>
          );
        })}
      </nav>
      <div className="sidebar-foot">v0.1 · M1 控制台</div>
    </aside>
  );
}
