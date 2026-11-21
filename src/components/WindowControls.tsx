import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

export function WindowControls() {
  const win = getCurrentWindow();
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    const current = getCurrentWindow();
    let revision = 0;
    let active = true;
    const sync = async () => {
      const request = ++revision;
      const value = await current.isMaximized();
      if (active && request === revision) setMaximized(value);
    };
    const listener = current.onResized(sync);
    void listener.then(sync);
    return () => {
      active = false;
      void listener.then((off) => off());
    };
  }, []);
  return (
    <div className="window-controls" role="group" aria-label="窗口控制">
      <button aria-label="最小化" title="最小化" onClick={() => void win.minimize()}>&#xE921;</button>
      <button aria-label={maximized ? "还原" : "最大化"} title={maximized ? "还原" : "最大化"} onClick={() => void win.toggleMaximize()}>{maximized ? "\uE923" : "\uE922"}</button>
      <button className="window-close" aria-label="关闭" title="关闭" onClick={() => void win.close()}>&#xE8BB;</button>
    </div>
  );
}
