# cliproxyapi-quota-dashboard

一个极简的 CLIProxyAPI 配额面板。使用有效的 API Key 登录后，可以集中查看实例中全部 OAuth 账号的用量和重置时间。

## 功能

- 支持 Claude、Codex、Gemini CLI 和 Kimi 配额展示
- 多账号配额聚合、用量百分比和重置倒计时
- API Key 登录，Management Key 仅保存在服务端
- 自动隐藏账号邮箱、用户名和凭据文件名
- 单页界面，无需额外前端服务

## Docker 运行

CLIProxyAPI 需要启用远程管理接口：

```yaml
remote-management:
  allow-remote: true
```

运行面板：

```bash
read -rsp "CLIProxyAPI management key: " CLIPROXY_MANAGEMENT_KEY
export CLIPROXY_MANAGEMENT_KEY

docker run -d \
  --name cliproxyapi-quota-dashboard \
  --restart unless-stopped \
  --add-host host.docker.internal:host-gateway \
  -p 8080:8080 \
  -e CLIPROXY_BASE_URL=http://host.docker.internal:8317 \
  -e CLIPROXY_MANAGEMENT_KEY \
  controlnet/cliproxyapi-quota-dashboard:latest

unset CLIPROXY_MANAGEMENT_KEY
```

打开 `http://localhost:8080`，输入任意有效的 CLIProxyAPI API Key 登录。

如果 CLIProxyAPI 不在本机，请把 `CLIPROXY_BASE_URL` 改成其实际地址。目前 Docker 镜像支持 `linux/amd64`。

> 任何持有有效 API Key 的用户都能查看该 CLIProxyAPI 实例中的全部账号配额，请勿将面板暴露到不可信网络。
