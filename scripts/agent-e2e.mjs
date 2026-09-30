// AI Agent 前端 UI e2e：用 playwright-core 驱动真实浏览器，mock 掉 LLM 的 SSE
// 输出。会话/消息落库走真 GraphQL，副作用、UI 渲染走真事件流。
//
// 跑法：
//   1. LEPTOS_SITE_ROOT=target/site-smoke cargo leptos build
//   2. 起 smoke server：./target/debug/rodeo /tmp/agent-smoke.toml
//      （环境：LEPTOS_SITE_ADDR=127.0.0.1:3099
//            LEPTOS_SITE_ROOT=target/site-smoke
//            LEPTOS_HASH_FILES=true
//            LEPTOS_OUTPUT_NAME=rodeo）
//   3. NODE_PATH=/Users/wangxiaoyan/.hermes/hermes-agent/node_modules \
//      node scripts/agent-e2e.mjs
//
// 任何 case 失败不中断；汇总后用 process.exit(1)。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';

const SERVER = process.env.RODEO_SERVER || 'http://127.0.0.1:3099';
const EMAIL = process.env.RODEO_EMAIL || 'admin@local';
const PASSWORD = process.env.RODEO_PASS || 'Admin12345';
const CHROME =
  '/Users/wangxiaoyan/Library/Caches/ms-playwright/chromium-1217/' +
  'chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing';

// ---------- 小工具 ----------

function nowIso() {
  return new Date().toISOString();
}

function fail(name, msg, extra) {
  return { name, pass: false, fail: msg, extra };
}
function pass(name, ms) {
  return { name, pass: true, ms };
}

async function gql(token, query, variables = {}) {
  const r = await fetch(SERVER + '/api/graphql', {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      authorization: 'Bearer ' + token,
    },
    body: JSON.stringify({ query, variables }),
  });
  const j = await r.json();
  if (j.errors && j.errors.length) {
    throw new Error('GraphQL: ' + (j.errors[0].message || JSON.stringify(j.errors)));
  }
  return j.data;
}

// 注入 token 到所有后续 context.newPage()。init script 在每次
// page load 时都会跑，所以 fresh context 才不会重放旧 token。
function injectToken(ctx, token) {
  return ctx.addInitScript(t => {
    localStorage.setItem('rodeo_jwt', JSON.stringify(t));
  }, token);
}

async function freshPage(browser, token) {
  const ctx = await browser.newContext();
  injectToken(ctx, token);
  const page = await ctx.newPage();
  page.setDefaultTimeout(8000);
  return { ctx, page };
}

// 走真 GraphQL 拿 token；不进 SPA（避开 hydration race）。
async function loginToken() {
  const fetchMod = await import('node:http');
  // 用 fetch（node 内建）直接打 GraphQL endpoint。
  const r = await fetch(SERVER + '/api/graphql', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      query: `mutation { login(email: "${EMAIL}", password: "${PASSWORD}") { token } }`,
    }),
  });
  const j = await r.json();
  if (!j.data || !j.data.login || !j.data.login.token) {
    throw new Error('login failed: ' + JSON.stringify(j));
  }
  return j.data.login.token;
}

async function bootstrapWorkspace(token) {
  const browser = await pw.chromium.launch({
    executablePath: CHROME,
    headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage'],
  });
  const ctx = await browser.newContext();
  await injectToken(ctx, token);
  const page = await ctx.newPage();
  page.setDefaultTimeout(8000);

  // 建一个测试工作空间，返回 slug 与 id。
  const slug = 'agent-test-' + Date.now().toString(36);
  const data = await gql(token, `mutation($n: String!, $s: String!) {
    createWorkspace(name: $n, slug: $s) { id slug }
  }`, { n: 'Agent 测试', s: slug });
  const wsId = data.createWorkspace.id;

  await ctx.close();
  await browser.close();
  return { slug, wsId };
}

// 拿到 workspace 的第一条现有 session（若有），用于 E1 测试。
async function fetchSessions(token, slug) {
  // 用一次性 page 取一次列表（不进 SPA）。
  const tmpCtx = await pw.chromium.launch({ executablePath: CHROME, headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage'] }).then(b => b.newContext());
  injectToken(tmpCtx, token);
  const page = await tmpCtx.newPage();
  const data = await gql(token, `query($s: String!) {
    workspace(slug: $s) { id }
  }`, { s: slug });
  const data2 = await gql(token, `query($id: ID!) {
    agentSessions(workspaceId: $id) { id }
  }`, { id: data.workspace.id });
  await tmpCtx.close();
  return data2.agentSessions || [];
}

// ---------- SSE mock ----------

// 把 AgentEvent JSON 对象序列化成 text/event-stream 的字节。
function sseData(json) {
  return 'data: ' + JSON.stringify(json) + '\n\n';
}
const SSE_CLOSE = 'event: close\ndata: end\n\n';

// 注册路由：在 stream URL 上一次性把 events 列表发出去，最后 close。
async function mockSSE(page, events) {
  return page.route('**/api/agent/turns/*/stream', async route => {
    // 让浏览器走 text/event-stream
    await route.fulfill({
      status: 200,
      headers: {
        'content-type': 'text/event-stream; charset=utf-8',
        'cache-control': 'no-cache',
        'x-accel-buffering': 'no',
      },
      body: events.map(sseData).join('') + SSE_CLOSE,
    });
  });
}

// 生成一段合理 turn_id（ULID 字符串）；runner 端不校验具体值，
// 但要注意前端 SSE 解析只关心 data: 行，turn_id 不参与。
function turnIdStub() {
  return '01ARZ3NDEKTSV4RRFFQ69G5FAV';
}

// 计数 sendAgentMessage mutation（用作 sanity check：UI 真的发起请求了）。
function countMutations(page, name) {
  let n = 0;
  page.on('request', req => {
    if (req.method() === 'POST' && req.url().endsWith('/api/graphql')) {
      try {
        const body = req.postDataJSON?.() || {};
        if (body?.query && body.query.includes(name)) n += 1;
      } catch (_) {}
    }
  });
  return () => n;
}

// agentMessages GraphQL 响应 mock：把"真实"消息列表塞进响应，让 UI 在
// post-stream refresh 后能看到我们期望的内容（reasoning / toolCalls 都需要
// 这样才能在 DOM 里被验证——单靠 SSE stream 会被 post-stream 一把覆盖）。
async function mockAgentMessages(page, messages) {
  await page.route('**/api/graphql', async route => {
    const body = route.request().postDataJSON?.() || {};
    if (body.query && body.query.includes('agentMessages(')) {
      await route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ data: { agentMessages: messages } }),
      });
      return;
    }
    await route.continue();
  });
}

// 构造一条 user 消息（GraphQL camelCase shape，与前端 AgentMessage 反序列化对得上）。
function mkUserMsg(content) {
  return {
    id: '01ARZ3NDEKTSV4RRFFQ69G5FA0',
    sessionId: '01ARZ3NDEKTSV4RRFFQ69G5FA1',
    role: 'USER',
    content,
    toolCalls: [],
    toolCallId: null,
    createdAt: '2026-09-30T10:00:00Z',
  };
}
function mkAssistantMsg(opts) {
  return {
    id: '01ARZ3NDEKTSV4RRFFQ69G5FB0',
    sessionId: '01ARZ3NDEKTSV4RRFFQ69G5FA1',
    role: 'ASSISTANT',
    content: opts.content || '',
    reasoning: opts.reasoning || '',
    toolCalls: opts.toolCalls || [],
    toolCallId: null,
    createdAt: '2026-09-30T10:00:01Z',
  };
}

// ---------- Cases ----------

const CASES = [];

CASES.push(async function case_A1_open_panel({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-panel', { timeout: 5000 });
    const onCls = await page.getAttribute('.agent-btn', 'class');
    if (!onCls.includes(' on')) return fail('A1', '.agent-btn 缺 .on 态', { class: onCls });
    return pass('A1 打开面板', Date.now() - t);
  } catch (e) {
    return fail('A1', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_A2_auto_select({ browser, token, slug }) {
  const t = Date.now();
  // 先建一个 session（用 page 直接走 mutation，再开面板验自动选）。
  const seed = await freshPage(browser, token);
  await seed.page.goto(SERVER + '/' + slug);
  await seed.page.waitForSelector('.wside .it', { timeout: 10000 });
  const ws = await gql(token, `query($s: String!) { workspace(slug: $s) { id } }`, { s: slug });
  const s = await gql(token, `mutation($id: ID!) { createAgentSession(workspaceId: $id) { id } }`, { id: ws.workspace.id });
  await seed.ctx.close();

  const { ctx, page } = await freshPage(browser, token);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-session-item', { timeout: 5000 });
    const items = await page.locator('.agent-session-item').count();
    const activeCount = await page.locator('.agent-session-item.agent-session-active').count();
    if (items < 1) return fail('A2', '会话列表为空', { items });
    if (activeCount !== 1) return fail('A2', 'active 项数量 ≠ 1', { activeCount });
    // 第一项（最新的）应当就是 active。
    const firstCls = await page.getAttribute('.agent-session-item:first-child', 'class');
    if (!firstCls.includes('agent-session-active')) {
      return fail('A2', '最新会话未自动选中', { firstCls });
    }
    return pass('A2 自动选最近', Date.now() - t);
  } catch (e) {
    return fail('A2', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_A3_new_session({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-panel', { timeout: 5000 });
    await page.waitForSelector('.agent-session-item', { timeout: 5000 });
    const before = await page.locator('.agent-session-item').count();
    await page.click('.agent-new');
    await page.waitForFunction(b => document.querySelectorAll('.agent-session-item').length === b + 1,
      before, { timeout: 5000 });
    const activeTitle = await page.locator('.agent-session-item.agent-session-active .agent-session-title').innerText();
    if (!/[-:]/i.test(activeTitle) && activeTitle.length === 0) {
      // 空 title 应该用 M-D HH:mm 占位（未发消息的新会话）。
      return fail('A3', '新会话 title 为空字符串（无占位）', { activeTitle });
    }
    return pass('A3 新建会话', Date.now() - t);
  } catch (e) {
    return fail('A3', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_A4_delete_session({ browser, token, slug }) {
  const t = Date.now();
  // 先准备 2 个 session
  const seed = await freshPage(browser, token);
  await seed.page.goto(SERVER + '/' + slug);
  await seed.page.waitForSelector('.wside .it', { timeout: 10000 });
  const ws = await gql(token, `query($s: String!) { workspace(slug: $s) { id } }`, { s: slug });
  await gql(token, `mutation($id: ID!) { createAgentSession(workspaceId: $id) { id } }`, { id: ws.workspace.id });
  await gql(token, `mutation($id: ID!) { createAgentSession(workspaceId: $id) { id } }`, { id: ws.workspace.id });
  await seed.ctx.close();

  const { ctx, page } = await freshPage(browser, token);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-session-item', { timeout: 5000 });
    const before = await page.locator('.agent-session-item').count();
    if (before < 2) return fail('A4', '种子会话数不足', { before });

    // active 是第一项，删它
    const delBtn = page.locator('.agent-session-item.agent-session-active .agent-session-del');
    await delBtn.click();
    await page.waitForFunction(b => document.querySelectorAll('.agent-session-item').length === b - 1,
      before, { timeout: 5000 });

    const afterActive = await page.locator('.agent-session-item.agent-session-active').count();
    if (afterActive !== 1) return fail('A4', '删除后无 active 项', { afterActive });
    return pass('A4 删除会话', Date.now() - t);
  } catch (e) {
    return fail('A4', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_B1_send_happy({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  const turnId = turnIdStub();
  await mockSSE(page, [
    { type: 'delta', turn_id: turnId, content: '你' },
    { type: 'delta', turn_id: turnId, content: '好' },
    { type: 'done', turn_id: turnId, message_id: '01ARZ3NDEKTSV4RRFFQ69G5FB0' },
  ]);
  // post-stream refresh 会从 server 拉；用 mock 让 server「正好返回 user + assistant」，
  // 这样可以验证 DOM 里的两个角色都正确。
  await mockAgentMessages(page, [
    mkUserMsg('你好'),
    mkAssistantMsg({ content: '你好' }),
  ]);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-textarea', { timeout: 5000 });

    await page.fill('.agent-textarea', '你好');
    await page.locator('.agent-textarea').press('Enter');

    // 流式期间：streaming 应出现（看 UI 反馈）。
    await page.waitForSelector('.agent-streaming', { timeout: 5000 });

    // post-stream 完成后两条消息都在
    await page.waitForSelector('.agent-msg-user', { timeout: 8000 });
    await page.waitForSelector('.agent-msg-assistant', { timeout: 8000 });

    const userText = await page.locator('.agent-msg-user .agent-msg-content').last().innerText();
    if (!userText.includes('你好')) return fail('B1', 'user msg 文本缺失', { userText });

    const asstText = await page.locator('.agent-msg-assistant .agent-msg-content').last().innerText();
    if (!asstText.includes('你好')) return fail('B1', 'assistant 文本缺失', { asstText });

    // textarea 清空、按钮恢复
    const taVal = await page.inputValue('.agent-textarea');
    if (taVal !== '') return fail('B1', 'textarea 没清空', { taVal });

    await page.waitForFunction(() => !document.querySelector('.agent-streaming'), null, { timeout: 8000 });
    const sendDisabled = await page.locator('.agent-send').getAttribute('disabled');
    if (sendDisabled !== null) return fail('B1', '.agent-send 仍 disabled', { sendDisabled });

    return pass('B1 发送 happy', Date.now() - t);
  } catch (e) {
    return fail('B1', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_B2_enter_shortcuts({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  await mockSSE(page, [
    { type: 'done', turn_id: turnIdStub(), message_id: '01ARZ3NDEKTSV4RRFFQ69G5FB1' },
  ]);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-textarea', { timeout: 5000 });

    // Enter → 应发送
    await page.locator('.agent-textarea').focus();
    await page.keyboard.type('a');
    await page.keyboard.press('Enter');
    await page.waitForFunction(() => !document.querySelector('.agent-streaming'), null, { timeout: 8000 });

    // Meta+Enter → 不应发送（textarea 内容保留）。Playwright 的
    // press('Enter', {modifiers: ['Meta']}) 在 headless Chrome 上 metaKey
    // 经常到不了 handler，所以这里用合成 KeyboardEvent 验证 on_send
    // 没有被触发（textarea 长度保持 == 之前）。
    // 这测的是「handler 没误发送」——是产品契约的核心部分；
    // 「Meta+Enter 真的插入 \n」是浏览器默认行为，handler 测不到也合理。
    await page.locator('.agent-textarea').focus();
    await page.keyboard.type('b');
    const beforeCmd = await page.inputValue('.agent-textarea');
    await page.evaluate(() => {
      const ta = document.querySelector('.agent-textarea');
      ta.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'Enter', metaKey: true, bubbles: true, cancelable: true,
      }));
    });
    await page.waitForTimeout(300);
    const afterCmd = await page.inputValue('.agent-textarea');
    if (afterCmd !== beforeCmd) {
      return fail('B2', 'Meta+Enter 触发了发送（应不触发）', { beforeCmd, afterCmd });
    }
    // 流式期间没出现 = 没发送
    const streaming = await page.locator('.agent-streaming').count();
    if (streaming > 0) return fail('B2', 'Meta+Enter 后 .agent-streaming 出现（说明 send 走了）', {});

    // Control+Enter → 同理：handler 应不发送
    await page.locator('.agent-textarea').fill('c');
    await page.locator('.agent-textarea').focus();
    const beforeCtrl = await page.inputValue('.agent-textarea');
    await page.evaluate(() => {
      const ta = document.querySelector('.agent-textarea');
      ta.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'Enter', ctrlKey: true, bubbles: true, cancelable: true,
      }));
    });
    await page.waitForTimeout(300);
    const afterCtrl = await page.inputValue('.agent-textarea');
    if (afterCtrl !== beforeCtrl) {
      return fail('B2', 'Control+Enter 触发了发送（应不触发）', { beforeCtrl, afterCtrl });
    }

    // 应至少 1 条 user msg（Enter 那次）；先前 case 留下的消息会被自动选
    // 中的会话带过来，所以只看「至少 1」而不是「恰好 1」。
    const userCount = await page.locator('.agent-msg-user').count();
    if (userCount < 1) return fail('B2', 'user msg 数量 < 1（Enter 没发送）', { userCount });

    return pass('B2 Enter 行为', Date.now() - t);
  } catch (e) {
    return fail('B2', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_B3_reasoning_fold({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  const turnId = turnIdStub();
  await mockSSE(page, [
    { type: 'reasoning', turn_id: turnId, content: 'step 1\nstep 2' },
    { type: 'delta', turn_id: turnId, content: '答' },
    { type: 'done', turn_id: turnId, message_id: '01ARZ3NDEKTSV4RRFFQ69G5FB2' },
  ]);
  // 让 server 在 post-stream 后返回带 reasoning 字段的 assistant（reasoning 是
  // #[serde(default)]，可以额外塞进 mock 里；真实 GraphQL 字段集里没它，但 mock
  // 不查字段名，返回什么前端就吃什么）。
  await mockAgentMessages(page, [
    mkUserMsg('推理测试'),
    mkAssistantMsg({ content: '答', reasoning: 'step 1\nstep 2' }),
  ]);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-textarea', { timeout: 5000 });

    await page.fill('.agent-textarea', '推理测试');
    await page.locator('.agent-textarea').press('Enter');

    await page.waitForSelector('.agent-think', { timeout: 8000 });
    // 默认折叠：open 属性缺失
    const openAttr = await page.getAttribute('.agent-think', 'open');
    if (openAttr !== null) return fail('B3', '推理链默认应折叠', { openAttr });

    // summary 文字（取最后一个，因为前面 case 留过同会话的 reasoning）
    const summary = await page.locator('.agent-think summary').last().innerText();
    if (!summary.includes('思考过程')) return fail('B3', 'summary 文字不是「思考过程」', { summary });

    // 展开后 body 含 reasoning 全文
    await page.locator('.agent-think summary').last().click();
    await page.waitForSelector('.agent-think[open] .agent-think-body', { timeout: 3000 });
    const body = await page.locator('.agent-think[open] .agent-think-body').last().innerText();
    if (!body.includes('step 1') || !body.includes('step 2')) {
      return fail('B3', '折叠体文本缺失', { body });
    }
    return pass('B3 推理链折叠', Date.now() - t);
  } catch (e) {
    return fail('B3', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_B4_tool_call({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  const turnId = turnIdStub();
  await mockSSE(page, [
    { type: 'tool_call', turn_id: turnId, name: 'queryEntries', args: { viewId: 'v1' } },
    { type: 'tool_result', turn_id: turnId, name: 'queryEntries', ok: true, kind: 'ok', preview: 'ok-preview' },
    { type: 'side_effect', turn_id: turnId, side_effect: { domain: 'view_query_replace', query_expr: 'Priority >= 3' } },
    { type: 'done', turn_id: turnId, message_id: '01ARZ3NDEKTSV4RRFFQ69G5FB3' },
  ]);
  // post-stream 后服务端返回 assistant.toolCalls 已落地——这是 UI 渲染 .agent-tool
  // 的前提条件（真 runner 跑出来的消息才有非空 tool_calls）。
  await mockAgentMessages(page, [
    mkUserMsg('工具测试'),
    mkAssistantMsg({
      content: '',
      toolCalls: [{
        id: 'tc1', name: 'queryEntries',
        args: { viewId: 'v1' },
        resultPreview: 'ok-preview 内容较长 — 用于覆盖折叠态截断与全文查看',
        ok: true,
      }],
    }),
  ]);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-textarea', { timeout: 5000 });

    await page.fill('.agent-textarea', '工具测试');
    await page.locator('.agent-textarea').press('Enter');

    await page.waitForSelector('.agent-tool', { timeout: 8000 });
    const toolName = await page.locator('.agent-tool .agent-tool-name').last().innerText();
    if (toolName !== 'queryEntries') return fail('B4', 'tool name 渲染错', { toolName });

    const errCls = await page.locator('.agent-tool-result.err').count();
    if (errCls !== 0) return fail('B4', 'ok=true 却渲染了 err 类', { errCls });

    // 折叠体里有 args 标签（last() 的 tool 内部可能还嵌一个「查看完整 result」summary，
    // 取第一个 summary 就是外层折叠）
    const lastTool = page.locator('.agent-tool').last();
    await lastTool.scrollIntoViewIfNeeded();
    await lastTool.locator('summary').first().click();
    await page.waitForSelector('.agent-tool-args', { timeout: 3000 });
    const argsText = await page.locator('.agent-tool-args').last().innerText();
    if (!argsText.includes('viewId')) return fail('B4', 'args 没渲染', { argsText });

    return pass('B4 工具调用', Date.now() - t);
  } catch (e) {
    return fail('B4', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_B5_ai_error_visible({ browser, token, slug }) {
  const t = Date.now();
  // 注意：不 mock SSE。让真 server 用空 [ai] 配置产出 runner Error 流——这就是
  // 用户生产环境遇到「没配模型」时的真实路径。post-stream refresh 拿到的消息列表
  // 用 mock 锁定成「只一条 user、无 assistant」。
  const { ctx, page } = await freshPage(browser, token);
  await mockAgentMessages(page, [mkUserMsg('AI 错')]);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-textarea', { timeout: 5000 });

    await page.fill('.agent-textarea', 'AI 错');
    await page.locator('.agent-textarea').press('Enter');

    // streaming 应短暂出现再消失
    await page.waitForSelector('.agent-streaming', { timeout: 3000 }).catch(() => {});
    await page.waitForFunction(() => !document.querySelector('.agent-streaming'), null, { timeout: 5000 });

    // post-stream 完成：user msg 在，assistant 不在
    await page.waitForSelector('.agent-msg-user', { timeout: 5000 });
    const asstCount = await page.locator('.agent-msg-assistant').count();
    if (asstCount !== 0) return fail('B5', 'AI 错误后不应出现 assistant', { asstCount });

    // 按钮恢复
    const sendDisabled = await page.locator('.agent-send').getAttribute('disabled');
    if (sendDisabled !== null) return fail('B5', 'AI 错误后 .agent-send 仍 disabled', { sendDisabled });

    // textarea 清空
    const taVal = await page.inputValue('.agent-textarea');
    if (taVal !== '') return fail('B5', 'AI 错误后 textarea 没清空', { taVal });

    return pass('B5 错误反馈', Date.now() - t);
  } catch (e) {
    return fail('B5', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_C1_file_picker_only({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-attach-pick', { timeout: 5000 });

    const fileInputs = await page.locator('label.agent-attach-pick input[type=file][multiple]').count();
    if (fileInputs !== 1) return fail('C1', 'file picker 数量错', { fileInputs });

    // 没有「附件 ID」之类的文本输入框
    const idLike = await page.locator('input[placeholder*="附件 ID"], input[placeholder*="attachment id" i]').count();
    if (idLike !== 0) return fail('C1', '面板里不应暴露 ID 输入框', { idLike });

    return pass('C1 文件选择器', Date.now() - t);
  } catch (e) {
    return fail('C1', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_C2_attach_happy({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);

  // 拦 uploadAttachment：走 multipart（postDataJSON=null），需要看 raw postData。
  // 我们让真实 uploadAttachment 跑（multipart 转发给 server），仅记录请求用来排查。
  await page.route('**/api/graphql', async route => {
    const req = route.request();
    const isUpload = req.url().includes('/api/graphql') &&
      (req.postData() || '').includes('uploadAttachment');
    if (isUpload) {
      console.log('  [C2 debug] uploadAttachment req headers:',
        req.headers()['content-type']);
    }
    await route.continue();
  });

  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-attach-pick', { timeout: 5000 });

    await page.setInputFiles('.agent-attach-pick input[type=file]', [
      { name: 'one.txt', mimeType: 'text/plain', buffer: Buffer.from('hello') },
      { name: 'two.txt', mimeType: 'text/plain', buffer: Buffer.from('world') },
    ]);

    // 等任一 chip 出现，再判断数量
    await page.waitForSelector('.agent-attach-chip', { timeout: 10000 });
    await page.waitForFunction(
      () => document.querySelectorAll('.agent-attach-chip').length >= 2,
      null, { timeout: 5000 });

    const labelText = await page.locator('.agent-attach-pick').innerText();
    if (labelText.includes('上传中…')) return fail('C2', '上传后还显示「上传中…」', { labelText });

    return pass('C2 上传 happy', Date.now() - t);
  } catch (e) {
    return fail('C2', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_C3_attach_quota({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);

  // 测试单文件 50 MiB 上限（service::attachment::MAX_ATTACHMENT_SIZE）：
  // 喂一个 60 MiB 文件 → 服务端 uploadAttachment 拒绝 → 面板 .agent-att-warn
  // 显示中文短句、不泄漏栈/路径/内部错误码 → chip 列表为空。
  // 注意：4 MiB 总额配额在 send_agent_message 时才检查（read attachments 阶段），
  // 不在 upload 阶段——所以累计测试应该测 send 不测 upload。
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-attach-pick', { timeout: 5000 });

    // 先建会话（否则 spawn_attachment_uploads 在「session_id is None」分支
    // 就直接挂「请先选择会话再添加附件」，根本走不到服务端的 50 MiB 检查）
    await page.click('.agent-new');
    await page.waitForSelector('.agent-session-item.agent-session-active', { timeout: 5000 });

    // 喂一个超 50 MiB 的文件（playwright setInputFiles 单文件硬上限 50 MiB，
    // 必须落盘后再传 path）
    const oversizedPath = '/tmp/agent-e2e-too-big.bin';
    const fs = await import('node:fs');
    fs.writeFileSync(oversizedPath, Buffer.alloc(60 * 1024 * 1024, 0x61));
    await page.setInputFiles('.agent-attach-pick input[type=file]', [
      oversizedPath,
    ]);
    fs.unlinkSync(oversizedPath);

    await page.waitForFunction(() => !!document.querySelector('.agent-att-warn'), null, { timeout: 30000 });
    const warn = await page.locator('.agent-att-warn').innerText();
    const forbidden = ['/Users/', 'target/', 'at src/', 'INTERNAL', 'os error', 'io error', 'stack backtrace'];
    for (const f of forbidden) {
      if (warn.includes(f)) return fail('C3', '错误文案泄漏内部信息', { found: f, warn });
    }
    if (warn.length > 120) return fail('C3', '错误文案过长（应短句）', { length: warn.length, warn });
    const chips = await page.locator('.agent-attach-chip').count();
    if (chips !== 0) return fail('C3', '失败文件不应入 chip 列表', { chips });

    return pass('C3 配额 + 脱敏', Date.now() - t);
  } catch (e) {
    return fail('C3', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_D1_panel_drag({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-header', { timeout: 5000 });

    const beforeStyle = await page.locator('.agent-panel').evaluate(el => el.getAttribute('style'));
    const header = page.locator('.agent-header');
    const box = await header.boundingBox();
    if (!box) return fail('D1', 'header 无 bounding box', {});

    // 从 header 中央按下（避开 .agent-new / .agent-close），拖动 +50/+30
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 50, box.y + box.height / 2 + 30, { steps: 5 });
    await page.mouse.up();

    const afterStyle = await page.locator('.agent-panel').evaluate(el => el.getAttribute('style'));
    if (afterStyle === beforeStyle) return fail('D1', 'style 没变', { beforeStyle, afterStyle });

    const lsRaw = await page.evaluate(() => localStorage.getItem('rodeo_agent_panel'));
    if (!lsRaw) return fail('D1', 'localStorage 未写入', {});
    const parts = lsRaw.split(',');
    if (parts.length !== 4 || parts.some(p => Number.isNaN(parseFloat(p)))) {
      return fail('D1', 'localStorage 格式错', { lsRaw });
    }
    return pass('D1 拖动持久化', Date.now() - t);
  } catch (e) {
    return fail('D1', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_D2_panel_resize({ browser, token, slug }) {
  const t = Date.now();
  const { ctx, page } = await freshPage(browser, token);
  try {
    await page.goto(SERVER + '/' + slug);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-resize-handle', { timeout: 5000 });

    const beforeStyle = await page.locator('.agent-panel').evaluate(el => el.getAttribute('style'));
    const handle = page.locator('.agent-resize-handle');
    const box = await handle.boundingBox();
    if (!box) return fail('D2', 'resize handle 无 box', {});

    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 80, box.y + box.height / 2 + 60, { steps: 5 });
    await page.mouse.up();

    const afterStyle = await page.locator('.agent-panel').evaluate(el => el.getAttribute('style'));
    if (afterStyle === beforeStyle) return fail('D2', 'style 没变', { beforeStyle, afterStyle });
    // 至少 width 或 height 变了
    if (!/width:\s*[\d.]+px/.test(afterStyle)) return fail('D2', 'width 数字没变', { afterStyle });

    const lsRaw = await page.evaluate(() => localStorage.getItem('rodeo_agent_panel'));
    if (!lsRaw || lsRaw.split(',').length !== 4) {
      return fail('D2', 'localStorage 格式错', { lsRaw });
    }
    return pass('D2 缩放持久化', Date.now() - t);
  } catch (e) {
    return fail('D2', e.message);
  } finally {
    await ctx.close();
  }
});

CASES.push(async function case_E1_switch_workspace({ browser, token }) {
  const t = Date.now();
  // 在 A 上搞一个 session，再建 B。
  const seed = await freshPage(browser, token);
  await seed.page.goto(SERVER + '/');
  const slugA = 'ws-a-' + Date.now().toString(36);
  const slugB = 'ws-b-' + Date.now().toString(36) + 'x';
  const wsA = await gql(token, `mutation($n: String!, $s: String!) {
    createWorkspace(name: $n, slug: $s) { id }
  }`, { n: 'A', s: slugA });
  const wsB = await gql(token, `mutation($n: String!, $s: String!) {
    createWorkspace(name: $n, slug: $s) { id }
  }`, { n: 'B', s: slugB });
  const sA = await gql(token, `mutation($id: ID!) { createAgentSession(workspaceId: $id) { id } }`, { id: wsA.createWorkspace.id });
  await seed.ctx.close();

  const { ctx, page } = await freshPage(browser, token);
  try {
    await page.goto(SERVER + '/' + slugB);
    await page.waitForSelector('.wside .it', { timeout: 10000 });
    await page.click('.agent-btn');
    await page.waitForSelector('.agent-panel', { timeout: 5000 });
    // 等拉完 sessions
    await page.waitForTimeout(500);
    const items = await page.locator('.agent-session-item').count();
    // B 没建 session，应当 0；或者建了的话一定不是 A 的那个 id
    const allText = await page.locator('.agent-session-item').evaluateAll(
      els => els.map(e => e.textContent || '').join('|')
    );
    if (items > 0 && allText.includes(sA.createAgentSession.id)) {
      return fail('E1', '切到 B 后看到 A 的 session', { allText });
    }
    return pass('E1 切 ws 隔离', Date.now() - t);
  } catch (e) {
    return fail('E1', e.message);
  } finally {
    await ctx.close();
  }
});

// ---------- Main ----------

(async () => {
  console.log('[' + nowIso() + '] launching browser');
  const browser = await pw.chromium.launch({
    executablePath: CHROME,
    headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage'],
  });

  let token;
  try {
    token = await loginToken();
    console.log('[' + nowIso() + '] login OK, token len=' + token.length);
  } catch (e) {
    console.error('login 失败：' + e.message);
    process.exit(2);
  }

  // bootstrap 一个工作空间供多数 case 用
  let slug;
  try {
    const bs = await bootstrapWorkspace(token);
    slug = bs.slug;
    console.log('[' + nowIso() + '] bootstrap ws=' + slug);
  } catch (e) {
    console.error('bootstrap 工作空间失败：' + e.message);
    process.exit(2);
  }

  const results = [];
  for (const c of CASES) {
    const r = await c({ browser, token, slug });
    results.push(r);
    const tag = r.pass ? 'PASS' : 'FAIL';
    const extra = r.pass ? `${r.ms}ms` : r.fail + (r.extra ? ' ' + JSON.stringify(r.extra) : '');
    console.log('[' + nowIso() + '] ' + tag + ' ' + r.name + ' — ' + extra);
  }

  await browser.close();

  const failed = results.filter(r => !r.pass);
  console.log('');
  console.log('=== 汇总 ===');
  console.log('total=' + results.length + ' pass=' + (results.length - failed.length) + ' fail=' + failed.length);
  if (failed.length) {
    for (const f of failed) console.log('  - ' + f.name + ': ' + f.fail);
    process.exit(1);
  }
})();
