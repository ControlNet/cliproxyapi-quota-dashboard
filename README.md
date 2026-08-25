# cliproxyapi-quota-dashboard

极简的单页配额面板：用 CLIProxyAPI 的 API Key 登录，查看该实例上**全部 OAuth 账号**的用量限额。

功能刻意保持最小：一个登录页 + 一个仪表盘页，没有别的。

![stack](https://img.shields.io/badge/backend-Rust%20%2B%20axum-orange) ![deps](https://img.shields.io/badge/frontend-vanilla%20single--file-blue)

## 工作原理

```
浏览器（单文件 SPA，无框架无构建）
   │  POST /api/login     ──► 用你的 API Key 调 CLIProxyAPI GET /v1/models 校验身份
   │  GET  /api/quota     ──► 服务端聚合：
   ▼
Rust 后端（单二进制，静态内嵌前端）
   │  GET  /v0/management/auth-files   列出所有 OAuth 凭据
   │  POST /v0/management/api-call     逐账号代理上游配额查询（$TOKEN$ 由 CLIProxyAPI 替换）
   ▼
CLIProxyAPI（需配置 remote-management）
```

- **用户侧只接触 API Key**：登录时经 `/v1/models` 验证，通过后签发 HMAC 签名的 HttpOnly 会话 Cookie（24h）。
- **Management key 只存在服务端**（环境变量），永不下发浏览器。
- 上游配额来源：Claude `oauth/usage`、Codex `wham/usage`、Gemini CLI `retrieveUserQuota`、Kimi `coding/v1/usages`，统一规范化为百分比窗口 + 重置倒计时。
- 服务端对聚合结果做 **20s 微缓存**；登录失败按 IP 限流（5 分钟内 8 次失败锁定）。

## 配置（环境变量）

| 变量 | 必填 | 说明 |
|---|---|---|
| `CLIPROXY_BASE_URL` | ✅ | CLIProxyAPI 地址，如 `http://127.0.0.1:8317` |
| `CLIPROXY_MANAGEMENT_KEY` | ✅ | CLIProxyAPI `config.yaml` 中 `remote-management.secret-key` |
| `PORT` | | 默认 `8080`，监听 `0.0.0.0` |

会话签名密钥会在每次启动时随机生成，因此服务重启后现有登录会话会自动失效。

> 远程访问 CLIProxyAPI 管理接口需要其配置里 `remote-management.allow-remote: true`。

## Docker 部署（推荐）

```bash
# .env（与 docker-compose.yml 同目录，勿提交仓库）
cat > .env <<'EOF'
CLIPROXY_BASE_URL=http://cliproxyapi:8317
CLIPROXY_MANAGEMENT_KEY=你的管理密钥
EOF

docker compose pull
docker compose up -d
# 打开 http://<host>:8080 ，输入任意有效的 CLIProxyAPI api-key 登录
```

Compose 默认拉取 `controlnet/cliproxyapi-quota-dashboard:latest`。如需固定版本，在 `.env` 中增加：

```dotenv
IMAGE_TAG=0.1.0
```

也可以直接运行镜像：

```bash
docker run -d --name quota-dashboard --restart unless-stopped \
  -p 8080:8080 \
  -e CLIPROXY_BASE_URL=http://cliproxyapi:8317 \
  -e CLIPROXY_MANAGEMENT_KEY=你的管理密钥 \
  controlnet/cliproxyapi-quota-dashboard:latest
```

镜像为多阶段构建：musl 静态编译 → `scratch` 运行层，最终镜像仅约 10 MB，运行内存占用约 10 MB。目前发布的镜像仅支持 `linux/amd64`。

## Docker 镜像发布

推送任意 Git tag 时，GitHub Actions 会构建并发布对应 tag 到 Docker Hub：

```bash
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

每次 tag push 都会发布原始 tag 并覆盖 `latest`。语义版本 tag 还会发布大版本别名，例如 `v2.3.1` 会发布 `v2.3.1`、`2` 和 `latest`。

仓库维护者需要在 GitHub Actions 中配置：

- Repository variable：`DOCKERHUB_USERNAME`
- Repository secret：`DOCKERHUB_TOKEN`（使用具有 Read/Write 权限的 Docker Hub access token，不要使用账号密码）

## 本地开发

```bash
# 终端 1：假 CLIProxyAPI（dev/ 目录仅为测试夹具，含虚构数据，不参与部署）
node dev/mock-cliproxy.mjs 9999
# 有效用户 key: sk-user-test-123 ；管理 key: mg-test-key

# 终端 2：dashboard
CLIPROXY_BASE_URL=http://127.0.0.1:9999 \
CLIPROXY_MANAGEMENT_KEY=mg-test-key \
PORT=8099 cargo run -r

# 打开 http://127.0.0.1:8099
```

### 验证命令

```bash
cargo check --all-targets        # 期望：无错误无警告
curl -s -X POST localhost:8099/api/login -H 'Content-Type: application/json' \
     -d '{"api_key":"sk-user-test-123"}' -c /tmp/cj.txt   # 期望 {"ok":true}
curl -s -b /tmp/cj.txt localhost:8099/api/quota | python3 -m json.tool   # 期望 accounts[...]
```

## API

| 方法/路径 | 说明 |
|---|---|
| `POST /api/login` `{"api_key"}` | 校验并签发会话 Cookie；401 无效 / 429 限流 |
| `POST /api/logout` | 清除会话 |
| `GET /api/session` | `{"authenticated":bool}`，页面路由用 |
| `GET /api/quota` | 聚合配额；未登录返回 401 |

`GET /api/quota` 响应中每个账号：

```jsonc
{
  "id": "...", "label": "Claude 工作组",
  //                ↑ 仅显示运营者自定义标签；未设置时自动匿名化为 "Claude #3"
  "provider": "claude|codex|gemini|kimi|other",
  "plan": "Max",            // 展示用徽章文本，可为 null
  "disabled": false,
  "windows": [              // 时间窗口或模型桶
    { "name": "5h", "used_percent": 42.5,
      "resets_at": "2026-08-26T02:00:00Z", "caption": null }
  ],
  "extra": {},              // 附加摘要，如额外用量、G1 credits
  "error": null             // 单账号级错误（不影响整体）
}
```

## 安全说明

- **上游身份不外泄**：账号邮箱、用户名、凭据文件名等字段不会下发到前端；卡片仅显示运营者自定义的 `label`，未设置时以「提供商 #编号」匿名展示。
- 会话 Cookie：HttpOnly + SameSite=Lax，签名常量时间校验。
- 登录限流：同 IP 5 分钟窗口内 8 次失败后拒绝 60 秒+。
- 后端对上游请求统一 15s 超时，账号间并发上限 4。
- 任何持有有效 api-key 的用户都能看到**所有**账号的配额——这是本工具的设计意图，请勿暴露到不可信网络。
