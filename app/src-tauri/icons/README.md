# 图标

Tauri 构建必须有一个 Windows 图标：`app/src-tauri/icons/icon.ico`。

本仓库不提交二进制图标，首次构建前请二选一：

1. **用自己的图标**（推荐）：准备一张 1024×1024 的 PNG，放到任意位置后执行

   ```powershell
   cd dev-sidecar-lite/app
   npx tauri icon ..\assets\icon.png
   ```

   Tauri 会自动生成 `src-tauri/icons/` 下的全套尺寸（含 `icon.ico`）。

2. **先用占位图跑通构建**：放一张任意 `icon.ico` 到本目录，构建会通过，只是托盘和安装包图标不好看。

`tauri.conf.json` 里只引用了 `icons/icon.ico`，其余尺寸按需在 `bundle.icon` 里追加。
