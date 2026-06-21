# API Nexus

API Nexus 是一个本地 AI 模型 API 网关，通过 OpenAI 兼容接口和 Anthropic 兼容接口统一转发、路由和组合模型请求。

## 功能特性

- OpenAI 兼容入口：`/v1/responses`、`/v1/chat/completions`、`/v1/completions`、`/v1/embeddings`、`/v1/models`
- Anthropic 兼容入口：`/v1/messages`、`/v1/messages/count_tokens`
- OpenAI Chat Completions 与 Anthropic Messages 双向协议转换
- 按模型配置供应商优先级，支持拖拽排序和失败自动切换
- 内置常用模型供应商预设
- 支持多个具名客户端 API Key，并按 Key 记录请求来源
- 使用 SQLite 持久化请求日志，支持保留策略和 CSV 导出
- 响应式用量趋势图和可展开的紧凑请求日志
- 按供应商配置模型价格，内置经过核对的预设价格、缓存价格和美元/人民币成本估算
- Fusion 多模型 Panel/Judge/Final 编排，可选本地网页搜索和页面抓取
- Codex 和 Claude Code 可将 `nexus/fusion` 直接作为 Agent 基模
- 使用 Windows DPAPI 保护上游和客户端 API Key
- 支持通过 GitHub Releases 签名更新
- 本地代理 API Key 保护和系统托盘后台运行
- 基于 Tauri 的桌面界面，提供响应式供应商和模型管理页面

## 本地开发

安装依赖并启动前端：

```bash
npm install
npm run dev
```

启动 Tauri 应用：

```bash
npm run tauri dev
```

构建应用：

```bash
npm run tauri build
```

## 验证

前端：

```bash
npm test
npm run test:e2e
npm run build
```

后端：

```bash
cd src-tauri
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

## 本地数据

API Nexus 将本地状态保存在 `%APPDATA%\api-nexus`：

- `config.json`：不包含密钥的普通配置
- `secrets.dpapi`：使用当前 Windows 用户凭据加密的 API Key
- `api-nexus.sqlite3`：持久化请求日志和用量历史

可以在“设置”中调整日志保留天数和最大记录数，也可以在“请求日志”页面导出 CSV。

## Fusion

Fusion 支持自由组合不同供应商的模型：

- 多个 Panel 模型并行分析
- Judge 模型比较各 Panel 的共识、分歧和遗漏
- Final 模型综合生成最终回答
- 按需模式可由 Outer Model 决定直接回答还是调用 Fusion

Fusion 代理提供两种模式：

- `forced`：每次请求都执行 Panel、Judge 和 Final
- `on_demand`：先请求 Outer Model，由它决定是否调用服务端 `fusion` 工具；也可以通过 OpenAI `tool_choice: "required"` 或 Anthropic `tool_choice: {"type":"any"}` 强制调用

Responses 和 Anthropic Messages 入口接受流式请求。API Nexus 会先完成内部 Fusion 编排，再发送符合对应协议的 SSE；OpenAI Chat Completions 的 Fusion 入口目前仍为非流式。

在 Codex 或 Claude Code 的 Agent 工具循环中，初始任务会执行完整 Fusion；后续纯 `tool_result` 轮次直接交给 Final（按需模式交给 Outer），不会在每次读取文件或执行命令后重复运行 Panel/Judge。需要重新分析包含工具历史的新用户任务时，API Nexus 会先将工具记录转换为普通文本再交给 Panel，避免不同供应商的推理字段不兼容。

### 网页工具

Fusion 的 Panel 和 Judge 模型可以通过本地 [open-webSearch](https://www.npmjs.com/package/open-websearch) 服务使用 `web_search` 和 `web_fetch`。需要单独启动该服务：

```bash
npx open-websearch serve
```

然后在 Fusion 页面启用网页工具，并填写命令输出的本地服务地址，例如 `http://127.0.0.1:3210`。出于安全考虑，API Nexus 只接受 `localhost`、`127.0.0.1` 或 `::1`。网页工具关闭或地址未配置时，Fusion 使用普通的单次模型请求。

## 客户端配置

OpenAI 兼容客户端：

```text
base_url = http://127.0.0.1:11434/v1
```

Anthropic 兼容客户端：

```text
ANTHROPIC_BASE_URL=http://127.0.0.1:11434
```

客户端 API Key 使用 API Nexus 中配置的代理密钥。

### Claude Code

Claude Code 可以通过 Anthropic Messages API 使用 Fusion，并支持 `Bash`、`Read`、`Edit`、`Write` 等内置客户端工具。在启动 Claude Code 前设置当前进程的环境变量：

```powershell
$env:ANTHROPIC_BASE_URL = "http://127.0.0.1:11434"
$env:ANTHROPIC_AUTH_TOKEN = "sk-nexus-your-key"
$env:ANTHROPIC_MODEL = "nexus/fusion"
claude
```

项目使用 Claude Code CLI `2.1.185` 进行了真实 CLI 集成测试，覆盖 Anthropic SSE `tool_use`、本地 `Bash` 执行和后续 `tool_result` 回传。Fusion 也会在本地处理 `/v1/messages/count_tokens`。

### Codex CLI

Codex 可以通过 Responses API 将 API Nexus Fusion 作为自定义模型供应商。在 Windows 的 `%USERPROFILE%\.codex\config.toml` 或其他系统的 `~/.codex/config.toml` 中添加：

```toml
model = "nexus/fusion"
model_provider = "api_nexus"

[model_providers.api_nexus]
name = "API Nexus Fusion"
base_url = "http://127.0.0.1:11434/v1"
env_key = "API_NEXUS_KEY"
wire_api = "responses"
```

启动 Codex 前设置 API Nexus 代理密钥：

```powershell
$env:API_NEXUS_KEY = "sk-nexus-your-key"
codex
```

项目使用 Codex CLI `0.141.0` 进行了真实 CLI 集成测试，覆盖客户端 `shell_command` 调用和后续 `function_call_output` 回传。Fusion 支持 Codex function、custom 以及 namespace 内的 function/custom 工具；OpenAI 托管工具不会转发给通用内部供应商。

## 自动发布

GitHub Actions 会在每次变更时执行前端测试、Playwright 冒烟测试、Rust 测试、格式检查和 Clippy。推送匹配 `v*` 的标签后，发布流水线会构建 Windows 安装包、生成签名更新文件、上传 `latest.json` 并创建 GitHub Release。

Tauri 更新签名使用以下仓库密钥：

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

Windows Authenticode 签名还需要：

- `WINDOWS_CERTIFICATE`：可信代码签名 PFX 的 Base64 内容
- `WINDOWS_CERTIFICATE_PASSWORD`：PFX 密码

如果未配置 Authenticode 密钥，流水线仍会发布未签名的 Windows 安装包；Tauri 应用内更新文件继续使用更新密钥签名。
