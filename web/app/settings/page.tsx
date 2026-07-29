"use client";

import { useSettings } from "@/components/SettingsContext";

export default function SettingsPage() {
  const { settings, setSettings } = useSettings();

  return (
    <div className="workspace settings-page">
      <header className="topbar">
        <h1>设置</h1>
        <span className="conn">全局偏好</span>
      </header>

      <section className="settings-body">
        <div className="setting-card">
          <h2>模型与输出</h2>

          <label className="setting-row">
            <div className="setting-info">
              <div className="setting-title">开启思考过程</div>
              <div className="setting-desc">
                开启后模型会输出思考过程（💭），可在任务输出中折叠查看；关闭则更简洁、出结果更快。
              </div>
            </div>
            <input
              type="checkbox"
              checked={settings.think}
              onChange={(e) => setSettings({ think: e.target.checked })}
            />
          </label>

          <label className="setting-row">
            <div className="setting-info">
              <div className="setting-title">思考过程默认展开</div>
              <div className="setting-desc">
                运行任务后，思考过程面板默认展开；关闭则默认折叠，需手动点开。
              </div>
            </div>
            <input
              type="checkbox"
              checked={settings.thoughtOpen}
              onChange={(e) => setSettings({ thoughtOpen: e.target.checked })}
            />
          </label>

          <div className="setting-row">
            <div className="setting-info">
              <div className="setting-title">工具调用最大轮数</div>
              <div className="setting-desc">
                工具调用模式下，模型最多调用工具并续答的轮数。开启思考后模型可能更"啰嗦"，建议 ≥ 5，避免撞上限报错。
              </div>
            </div>
            <input
              type="number"
              className="setting-num"
              min={1}
              max={20}
              value={settings.maxRounds}
              onChange={(e) => {
                const v = parseInt(e.target.value, 10);
                setSettings({ maxRounds: Number.isFinite(v) ? Math.min(20, Math.max(1, v)) : 1 });
              }}
            />
          </div>
        </div>

        <p className="settings-note">
          以上设置会自动保存在本机浏览器（localStorage），刷新或重开页面后依然生效。
        </p>
      </section>
    </div>
  );
}
