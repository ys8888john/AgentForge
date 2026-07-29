"use client";

import {
  createContext,
  useContext,
  useEffect,
  useState,
  ReactNode,
} from "react";

// 全局设置：目前包含「思考过程」相关开关与工具调用轮数。
// 存放在浏览器 localStorage，刷新 / 重开页面后依然生效，并在工作台与
// 设置页之间共享（通过 React Context）。
export interface Settings {
  /** 是否开启模型思考（reasoning），后端透传为 Thought 事件 */
  think: boolean;
  /** 思考过程面板在任务输出中默认是否展开 */
  thoughtOpen: boolean;
  /** 工具调用模式下，模型最多调用工具并续答的轮数 */
  maxRounds: number;
}

const DEFAULTS: Settings = {
  think: true,
  thoughtOpen: true,
  maxRounds: 5,
};

const STORAGE_KEY = "agentos.settings";

interface Ctx {
  settings: Settings;
  setSettings: (patch: Partial<Settings>) => void;
}

const SettingsContext = createContext<Ctx | null>(null);

export function SettingsProvider({ children }: { children: ReactNode }) {
  const [settings, setS] = useState<Settings>(DEFAULTS);

  // 挂载时从 localStorage 恢复（同构渲染时服务端/客户端都用 DEFAULTS，
  // 待客户端挂载后再读取，避免 hydration 不一致）。
  useEffect(() => {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      try {
        const parsed = JSON.parse(raw);
        setS({ ...DEFAULTS, ...parsed });
      } catch {
        /* 损坏则忽略，沿用默认 */
      }
    }
  }, []);

  function setSettings(patch: Partial<Settings>) {
    setS((prev) => {
      const next = { ...prev, ...patch };
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
      return next;
    });
  }

  return (
    <SettingsContext.Provider value={{ settings, setSettings }}>
      {children}
    </SettingsContext.Provider>
  );
}

export function useSettings(): Ctx {
  const ctx = useContext(SettingsContext);
  if (!ctx) {
    throw new Error("useSettings 必须在 <SettingsProvider> 内使用");
  }
  return ctx;
}
