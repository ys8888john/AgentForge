// 侧边栏：面板导航（宝塔风格）。M1 仅"工作台"可用，其余为占位。
export default function Sidebar() {
  return (
    <aside className="sidebar">
      <div className="logo">agentOS</div>
      <nav>
        <a className="nav-item active" href="/">
          工作台
        </a>
        <a className="nav-item" href="/sessions">
          会话
        </a>
        <a className="nav-item" href="/agents">
          智能体
        </a>
        <a className="nav-item" href="/settings">
          设置
        </a>
      </nav>
      <div className="sidebar-foot">v0.1 · M1 控制台</div>
    </aside>
  );
}
