// TEST FIXTURE — fake CLIProxyAPI server for local development only.
// NOT shipped in the Docker image (.dockerignore excludes dev/).
// All accounts/quota numbers below are DUMMY DATA for testing.
//
// Usage: node dev/mock-cliproxy.mjs [port]   (default 9999)
// Valid user key:  sk-user-test-123
// Management key:  mg-test-key

import http from 'node:http';

const PORT = Number(process.argv[2] || 9999);
const USER_KEY = 'sk-user-test-123';
const MGMT_KEY = 'mg-test-key';

const FUTURE = (secs) => new Date(Date.now() + secs * 1000).toISOString();

const AUTH_FILES = [
  {
    id: 'file-claude-1', auth_index: '0', name: 'claude-max.json',
    type: 'claude', provider: 'claude', label: 'Claude 工作组',
    email: 'dev-team@example.com', status: 'active', disabled: false,
  },
  {
    id: 'file-codex-1', auth_index: '1', name: 'codex-plus.json',
    type: 'codex', provider: 'codex', label: null,
    account: 'plus-user@example.com', status: 'active', disabled: false,
    id_token: { chatgpt_account_id: 'acc-7f2a', chatgpt_plan_type: 'plus' },
  },
  {
    id: 'file-gemini-1', auth_index: '2', name: 'gemini-free.json',
    type: 'gemini-cli', provider: 'gemini-cli', label: null,
    account: 'someone@gmail.com (gen-lang-client-042)', status: 'active', disabled: false,
  },
  {
    id: 'file-kimi-1', auth_index: '3', name: 'kimi-main.json',
    type: 'kimi', provider: 'kimi', label: 'Kimi 主力号',
    account: 'kimi@example.com', status: 'active', disabled: false,
  },
  {
    id: 'file-qwen-1', auth_index: '4', name: 'qwen-api.json',
    type: 'qwen', provider: 'qwen', label: 'Qwen API',
    account: 'qwen@example.com', status: 'active', disabled: false,
  },
  {
    id: 'file-claude-2', auth_index: '5', name: 'claude-old.json',
    type: 'claude', provider: 'claude', label: '旧 Claude 号',
    email: 'old@example.com', status: 'disabled', disabled: true,
  },
];

// Per-provider upstream quota payloads (realistic shapes from research).
function upstreamBody(url) {
  if (url === 'https://api.anthropic.com/api/oauth/usage') {
    return {
      five_hour: { utilization: 42.5, resets_at: FUTURE(4 * 3600 + 800) },
      seven_day: { utilization: 12.3, resets_at: null },
      extra_usage: { monthly_limit: 10000, used_credits: 2530 },
    };
  }
  if (url === 'https://api.anthropic.com/api/oauth/profile') {
    return { account: { has_claude_max: true, has_claude_pro: false } };
  }
  if (url === 'https://chatgpt.com/backend-api/wham/usage') {
    return {
      plan_type: 'plus',
      rate_limit: {
        primary_window: { used_percent: 61.5, limit_window_seconds: 18000, reset_after_seconds: 5400 },
        secondary_window: { used_percent: 23.8, limit_window_seconds: 604800, reset_after_seconds: 345600 },
      },
    };
  }
  if (url.startsWith('https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota')) {
    return {
      buckets: [
        { modelId: 'gemini-2.5-pro', tokenType: 'PROMPT', remainingFraction: 0.368, remainingAmount: 368, resetTime: FUTURE(5400) },
        { modelId: 'gemini-2.5-flash', remainingFraction: 0.851, remainingAmount: 8510, resetTime: FUTURE(600) },
      ],
    };
  }
  if (url.startsWith('https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist')) {
    return {
      currentTier: {
        id: 'standard-tier',
        availableCredits: [{ creditType: 'GOOGLE_ONE_AI', creditAmount: 32 }],
      },
    };
  }
  if (url === 'https://api.kimi.com/coding/v1/usages') {
    return {
      limits: [
        { title: '月度额度', detail: { used: 120, limit: 300, remaining: 180, resetAt: FUTURE(5 * 86400) } },
        { name: '五小时窗口', detail: { used: 40, limit: 50, remaining: 10, resetIn: 2100 } },
      ],
    };
  }
  return null;
}

function json(res, status, obj) {
  const body = JSON.stringify(obj);
  res.writeHead(status, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) });
  res.end(body);
}

function readBody(req) {
  return new Promise((resolve) => {
    let data = '';
    req.on('data', (c) => { data += c; });
    req.on('end', () => resolve(data));
  });
}

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  const auth = req.headers['authorization'] || '';
  console.log(`[mock] ${req.method} ${url.pathname}`);

  // --- user-facing endpoint ---
  if (url.pathname === '/v1/models') {
    if (auth === `Bearer ${USER_KEY}`) return json(res, 200, { data: [{ id: 'gpt-5' }, { id: 'claude-sonnet-4' }] });
    return json(res, 401, { error: 'invalid api key' });
  }

  // --- management endpoints ---
  if (auth !== `Bearer ${MGMT_KEY}`) return json(res, 401, { error: 'unauthorized' });

  if (url.pathname === '/v0/management/auth-files') {
    return json(res, 200, { files: AUTH_FILES });
  }

  if (url.pathname === '/v0/management/api-call' && req.method === 'POST') {
    const payload = JSON.parse((await readBody(req)) || '{}');
    const bodyObj = upstreamBody(payload.url || '');
    if (bodyObj == null) {
      return json(res, 200, { status_code: 404, header: {}, body: JSON.stringify({ error: 'no mock for this url' }) });
    }
    // CLIProxyAPI returns the upstream body as a STRING inside the envelope.
    return json(res, 200, { status_code: 200, header: {}, body: JSON.stringify(bodyObj) });
  }

  return json(res, 404, { error: 'not found' });
});

server.listen(PORT, () => console.log(`[mock] fake CLIProxyAPI listening on :${PORT}`));
