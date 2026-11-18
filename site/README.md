# Soyo Harness Site

Soyo Harness 的静态展示页，视觉语言与 `docs/design/desktop-ui.md`（D-075/D-076 目标规范）一致：Soyo Dark / Soyo Light 双主题、Source Serif 4 展示字体（仅 Latin 子集）、lucide 图标与 `.soyo-*` 组件样式。

技术栈与 `desktop/` 保持一致：Vite + React 19 + TypeScript + Tailwind CSS v4 + lucide-react。

## 开发

```sh
pnpm install
pnpm dev        # http://127.0.0.1:1421
pnpm build      # 输出到 dist/
pnpm preview    # 预览生产构建
```

## 部署（Cloudflare Pages）

| 配置项 | 值 |
| --- | --- |
| Build command | `pnpm install && pnpm build` |
| Build output directory | `dist` |
| Root directory | `site` |
| Node 版本 | `NODE_VERSION=22`（环境变量，可选） |

单页静态站点，无路由与 SSR 依赖；`pnpm build` 产物可直接托管。

## 约定

- 主题：System / Light / Dark 三档，生效值缓存在 `localStorage["soyo.theme"]`（与 D-076 键名一致），偏好在 `soyo.theme.pref`。
- 语言：en / zh-CN，默认跟随浏览器，偏好在 `soyo.lang`。
- 色彩与组件只使用 CSS 变量与 `.soyo-*` 类；新增视觉元素先对照 desktop-ui.md §8 的 Token 表。
- 应用图标复用 `desktop/public/app-icon.png`，更新时同步拷贝。
