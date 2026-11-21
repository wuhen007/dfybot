# Aurabot Rust

Aurabot Rust 是一个独立运行的 QQ 机器人服务端，使用 Rust 编写，不依赖 DeepSeek Harness。OneBot v11 与 QQ 官方开放平台共用同一套聊天引擎、记忆和权限系统。

现阶段支持聊天、逐会话中断、人格与心情、语音识别和语音回复、贴图、长期记忆、私聊主动对话、权限控制、网页管理面板和可选桌宠。QQ 空间、B 站、本地翻唱、图像生成、任意本地 HTTP 接口和第三方插件市场不在当前范围内。

## 构建与启动

安装 Rust stable 和当前平台所需的 C/C++ 链接器。若要通过 OneBot 发送语音，还需要安装 `ffmpeg` 和 SILK 编码器。

```powershell
cargo test
cargo build --release
```

初始化配置并检查：

```powershell
.\target\release\aurabot.exe init
```

编辑生成的 `config.toml`，设置模型服务的 API Key 环境变量，然后先用 dry-run 模式启动。dry-run 会记录预期回复，但不会发给 QQ 用户。

```powershell
$env:AURABOT_MODEL_KEY = "你的模型 API Key"
.\target\release\aurabot.exe check
.\target\release\aurabot.exe serve
```

管理面板地址由 `server.listen` 决定。初始化生成的默认配置使用 `127.0.0.1:8787`；仓库内示例 `config.toml` 使用 `127.0.0.1:8788`。首次启动时，管理令牌会写入 `data/admin.token`；也可以通过 `AURABOT_ADMIN_TOKEN` 环境变量提供令牌。

服务默认只监听本机回环地址。若要接收 QQ 官方平台 Webhook，需要将服务绑定到可从公网访问的地址，并在反向代理配置 TLS。不要将管理 API 暴露到未受保护的公网。

## 接入 OneBot v11

在 OneBot 实现端配置反向 WebSocket，连接到 `onebot.ws_url`；启用 `onebot.enabled`，填写机器人自己的 QQ 号，并将 `onebot.http_url` 设置为该实现端的 HTTP API 根地址。可通过 `onebot.token_env` 指定读取访问令牌的环境变量。

当前支持群聊、私聊、图片、贴图和语音记录。收到语音后，会通过配置的 OpenAI 兼容 ASR 服务转成文字。OneBot 语音回复需要：

- `voice.enabled = true`，并设置语音服务 API Key。
- 安装 `ffmpeg`。
- 将 `voice.silk_encoder` 配置为 SILK 编码器程序。程序应从标准输入读取单声道、24 kHz WAV，并向标准输出写入 SILK 数据。
- 将 `chat.voice_replies = true` 打开。

语音下载受 `voice.max_bytes` 限制，且媒体 URL 必须使用 HTTPS，并且主机名需要列入 `voice.media_hosts`。

## 接入 QQ 官方机器人

在 QQ 开放平台创建机器人，并配置 `official.app_id`、`official.secret_env` 及对应的 App Secret 环境变量。根据平台已授予的权限选择场景和事件。

支持两种传输方式：

- Webhook：设置 `official.transport = "webhook"`，将平台回调地址指向 `https://你的域名/official/webhook`，并转发到本服务。服务会处理 URL 验证，并在事件入队前校验 Ed25519 签名。
- WebSocket：设置 `official.transport = "websocket"`，服务会主动连接 QQ Gateway，并在连接中断后重连。此方式只需要服务能够访问外网。

官方平台是否允许某个场景或事件由 QQ 平台权限决定。官方账号标识是平台提供的不透明 ID，与 OneBot 的 QQ 号码分开保存。当前官方语音回复要求 TTS 输出格式为 Opus；媒体上传回复已实现群聊和 C2C 私聊场景，其他场景会退回文字回复。

## 语音服务

ASR 和 TTS 使用 OpenAI 兼容接口。可通过 `voice.base_url`、`voice.asr_model`、`voice.tts_model`、`voice.tts_voice` 和 `voice.tts_format` 配置服务与模型，并通过 `voice.api_key_env` 指定 API Key 环境变量。`chat.voice_replies` 控制是否优先用语音发送回复；不可用时会回退为文本。

本项目只实现机器人听取语音和发送语音，不包含本地翻唱功能。

## 管理与数据

管理面板使用 Bearer Token 保护。建议先保持 `chat.enabled = false` 和 `chat.dry_run = true`，确认模型、平台连接、回复风格和权限后再开启实际发送。

将稳定的平台用户 ID 配置到 `chat.owners` 后再启用管理员命令。OneBot 使用 QQ 号；官方机器人使用平台 OpenID。群聊默认需要明确提及机器人。聊天记录、记忆与会话按平台和场景隔离。SQLite 使用 WAL，历史记录、队列、上下文、回复长度、媒体大小和记忆容量均有上限；管理面板提供数据库备份与恢复。

主动对话仅面向配置好的主人私聊会话，支持清醒时段、轮换目标和每日晚安消息，不会主动给群聊发送消息。

模型可以搜索会话记忆和贴图，并在权限允许时保存记忆。服务端不会执行模型生成的 Shell 命令、任意 HTTP 请求或下载的插件。

## 可选桌宠

桌宠是单独的可选程序，不会和服务端共用进程：

```powershell
cargo build --release --features desktop-pet --bin aurabot-pet
.\target\release\aurabot-pet.exe --server http://127.0.0.1:8788 --token-file data/admin.token
```

将示例中的端口替换为 `server.listen` 配置的实际端口。桌宠每 3 秒请求一次受令牌保护的 `/api/pet/state`，只接收主人会话的汇总心情和活动状态，不接收聊天内容。角色动画使用仓库内 Shenshen 素材，素材选择与许可记录见 [素材说明](assets/shenshen/README.md)。发布时请保留 `assets/shenshen` 目录结构和许可文件。

本机回环地址可以使用 HTTP；远程服务地址必须使用 HTTPS。

## 验证与当前限制

运行 `cargo fmt --check`、`cargo test` 和 `cargo build --release` 验证服务端。桌宠可通过上面的命令单独构建。真实平台联调需要配置各自的凭据和 QQ 沙箱；官方平台各场景及媒体链路仍需按实际获批权限验证。OneBot 语音输出需要与本地 SILK 编码器配合测试。

当前项目仍在开发中，不能视为已完成全部生产环境验证。原项目采用 MIT 许可证；本重构保持 MIT 许可并注明产品概念来源，同时独立实现代码。

## 许可证

MIT，详见 [LICENSE](LICENSE)。
