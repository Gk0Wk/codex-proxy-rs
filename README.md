# PR #560 页面验收证据

这是个人 fork 的截图证据分支，供 zyycn/codex-proxy-rs PR #560 远程审阅，不作为宿主源码合入

截图来自本地运行的真实 AccountEditModal、AccountSettingsFields 与 AccountModelAccessField，外围为临时预览壳，账号、分组、API URL、模型目录和时间均为合成数据，未使用真实密钥或线上账号

- before-light.png：基线 aa0ce520，1280×1200，浅色
- after-light.png：变更后，1280×1200，浅色
- after-dark.png：变更后，1280×1200，深色
- after-en.png：变更后，1280×1200，英文
- after-mobile.png：变更后，390×844，手机视口，已滚动到模型限制区域

变更后的源码提交见 manifest.json，逐文件 SHA-256 见 SHA256SUMS

实际覆盖：缓存读取、手动刷新观察时间、切换账号清除旧观察时间、未知时间、空目录保留已选 ID、目录错误提示与恢复、黑白名单和不限切换、浅色/深色/英文/手机布局

临时预览的 HTTP fixture 只绑定 loopback，本证据不代表线上部署或真实模型端点的集成测试

## 更易阅读的前后对比

- before-focus.png：基线 aa0ce520，720×900，浅色，真实浏览器运行截图
- after-focus.png：源码 054681aff3f5f2c3d8520332c7a049673943aea9，720×900，浅色，真实浏览器运行截图
- 两张使用相同的合成账号和目录，不改动样式、不后期编辑，保留弹窗与相邻字段上下文
- 具体差异在“模型限制”下方：变更后新增目录获取时间，以及目录列出模型 ID 不等于端点已验证的说明；候选模型列表不变
