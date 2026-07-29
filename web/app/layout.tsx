import "./globals.css";
import Sidebar from "@/components/Sidebar";
import { SettingsProvider } from "@/components/SettingsContext";

export const metadata = {
  title: "agentOS 控制台",
  description: "agentOS 智能体管理面板",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="zh-CN">
      <body>
        <SettingsProvider>
          <div className="app-shell">
            <Sidebar />
            <main className="main">{children}</main>
          </div>
        </SettingsProvider>
      </body>
    </html>
  );
}
