// EntryTable 标题列右键菜单的 WebKit 验证：与侧栏 vmenu 共享同一套
// close_vmenu 闭包，dc3e968 的兜底链也要覆盖这条路径。
//
// 跑法同 view-config-menu-webkit-e2e.mjs（沿用同一套 smoke server）。

import pw from '/Users/wangxiaoyan/.hermes/hermes-agent/node_modules/playwright-core/index.js';

const SERVER = process.env.RODEO_SERVER || 'http://127.0.0.1:3099';
const EMAIL = process.env.RODEO_EMAIL || 'admin@local';
const PASSWORD = process.env.RODEO_PASS || 'Admin12345';

function nowIso() { return new Date().toISOString(); }
function fail(name, msg, extra) { return { name, pass: false, fail: msg, extra }; }
function pass(name, ms) { return { name, pass: true, ms }; }

async function gql(token, query, variables = {}) {
  const r = await fetch(SERVER + '/api/graphql', {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: 'Bearer ' + token },
    body: JSON.stringify({ query, variables }),
  });
  const j = await r.json();
  if (j.errors && j.errors.length) throw new Error('GraphQL: ' + (j.errors[0].message || JSON.stringify(j.errors)));
  return j.data;
}
async function loginToken() {
  const r = await fetch(SERVER + '/api/graphql', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ query: `mutation { login(email: "${EMAIL}", password: "${PASSWORD}") { token } }` }),
  });
  const j = await r.json();
  if (!j.data || !j.data.login || !j.data.login.token) throw new Error('login failed: ' + JSON.stringify(j));
  return j.data.login.token;
}
async function bootstrap(token) {
  const slug = 'rc' + Date.now().toString(36).slice(-8);
  const ws = await gql(token,
    'mutation($n:String!,$s:String!){ createWorkspace(name:$n,slug:$s){id slug} }',
    { n: 'Entry VMenu E2E', s: slug });
  const wsId = ws.createWorkspace.id;
  // 建 3 个 entry，让 table 有可点的行
  const entries = [];
  for (let i = 0; i < 3; i++) {
    const e = await gql(token,
      'mutation($w:ID!,$t:String!){ createEntry(workspaceId:$w, title:$t){ code } }',
      { w: wsId, t: `entry-${i+1}` });
    entries.push(e.createEntry.code);
  }
  return { slug, wsId, entries };
}
function injectTokenCtx(ctx, token) {
  return ctx.addInitScript((t) => { localStorage.setItem('rodeo_jwt', JSON.stringify(t)); }, token);
}

(async () => {
  console.log('[' + nowIso() + '] launching webkit');
  const pwCore = pw.default || pw;
  const browser = await pwCore.webkit.launch({ headless: true });
  let token;
  try {
    token = await loginToken();
    console.log('[' + nowIso() + '] login OK, token len=' + token.length);
  } catch (e) { console.error('login failed: ' + e.message); process.exit(2); }
  let bs;
  try {
    bs = await bootstrap(token);
    console.log(`[${nowIso()}] bootstrap ws=${bs.slug} entries=${bs.entries.join(',')}`);
  } catch (e) { console.error('bootstrap failed: ' + e.message); process.exit(2); }

  const ctx = await browser.newContext();
  injectTokenCtx(ctx, token);
  // 跳过 clipboard 权限授予（webkit 不识别 clipboard-write，会让 context 进入坏状态）；
  // 验证 vmenu 行为本身不需要读剪贴板。
  const page = await ctx.newPage();

  // composed_path 探针
  await page.addInitScript(() => {
    window.__vmenuMousedownLogs = [];
    document.addEventListener('mousedown', (ev) => {
      const path = ev.composedPath().map(n => {
        if (n.nodeType === 1) return `${n.tagName}.${n.className || '(no class)'}`;
        if (n.nodeType === 3) return `#text("${n.textContent.slice(0, 20)}")`;
        if (n.nodeType === 9) return '#document';
        return `#nodeType${n.nodeType}`;
      });
      window.__vmenuMousedownLogs.push({ target: ev.target.tagName || ev.target.nodeName, path });
    }, true);
  });

  try {
    await page.goto(`${SERVER}/${bs.slug}`, { waitUntil: 'commit', timeout: 60000 });
  } catch (e) { console.warn('[goto] timeout, continuing'); }
  await page.waitForTimeout(25000);
  // 等 table 出现 .title-cell 行
  await page.waitForFunction(
    () => document.querySelectorAll('.tbl .title-cell, .tbl tbody tr td.title-cell').length >= 3,
    { timeout: 120000, polling: 500 },
  ).catch(async (e) => {
    const debug = await page.evaluate(() => {
      const tbls = Array.from(document.querySelectorAll('.tbl'));
      return tbls.map(t => ({ class: t.className, rows: t.querySelectorAll('tbody tr').length }));
    });
    console.error('[debug]', JSON.stringify(debug));
    throw e;
  });

  // 清空探针日志
  await page.evaluate(() => { window.__vmenuMousedownLogs = []; });

  const t0 = Date.now();
  const results = [];
  for (let cyc = 0; cyc < 20; cyc++) {
    for (let eIdx = 0; eIdx < bs.entries.length; eIdx++) {
      const code = bs.entries[eIdx];
      // 找标题单元：按 entry 在 eIdx 位置找（bootstrap 时 entry-N 顺序建了 N 个 entry）
      const titleBox = await page.evaluate((idx) => {
        const cells = Array.from(document.querySelectorAll('.tbl tbody tr td.title-cell'));
        const target = cells[idx];
        if (!target) return null;
        const b = target.getBoundingClientRect();
        return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
      }, eIdx);
      if (!titleBox) {
        results.push({ cyc, eIdx, code, ok: false, why: 'title cell not found' });
        continue;
      }
      // 右键
      await page.mouse.click(titleBox.x, titleBox.y, { button: 'right' });
      await page.waitForTimeout(200);
      // vmenu 应出现，含「复制 code」和「删除 entry」
      const menuState = await page.evaluate(() => {
        const ms = document.querySelectorAll('.vmenu');
        return ms.length === 0 ? null : {
          buttons: Array.from(ms[0].querySelectorAll('button')).map(b => b.innerText.trim()),
        };
      });
      if (!menuState) {
        results.push({ cyc, eIdx, code, ok: false, why: 'no vmenu' });
        continue;
      }
      const hasCopyCode = menuState.buttons.some(t => t.includes('复制编码') || t.includes('Copy code') || t.includes('Copiar código') || t.includes('コードをコピー'));
      const hasDelete = menuState.buttons.some(t => t.includes('删除') || t.includes('Delete') || t.includes('Eliminar'));
      // 三档时间点探针：vmenu 应当稳定（不被 close_vmenu 误关）
      const t100 = await page.evaluate(() => document.querySelectorAll('.vmenu').length);
      await page.waitForTimeout(300);
      const t500 = await page.evaluate(() => document.querySelectorAll('.vmenu').length);
      await page.waitForTimeout(1000);
      const t1500 = await page.evaluate(() => document.querySelectorAll('.vmenu').length);
      const ok = t100 > 0 && t500 > 0 && t1500 > 0 && hasCopyCode && hasDelete;
      const why = !ok ? `t100=${t100} t500=${t500} t1500=${t1500} copyCode=${hasCopyCode} delete=${hasDelete}` : '';
      results.push({ cyc, eIdx, code, ok, why });
      // 关菜单：按 Escape
      await page.keyboard.press('Escape').catch(() => {});
      await page.waitForTimeout(100);
      // Escape 可能不关 vmenu（取决于实现），强制点一下空白
      await page.mouse.click(2, 2);
      await page.waitForTimeout(100);
    }
  }

  // 收集 composed_path 探针
  const probes = await page.evaluate(() => (window.__vmenuMousedownLogs || []).slice(0, 5));

  const total = results.length;
  const passCnt = results.filter(r => r.ok).length;
  const failed = results.filter(r => !r.ok);

  await browser.close();

  console.log('');
  console.log('=== 汇总 ===');
  console.log(`total=${total} pass=${passCnt} fail=${failed.length} time=${Date.now() - t0}ms`);
  for (let e = 0; e < bs.entries.length; e++) {
    const er = results.filter(x => x.eIdx === e);
    const ep = er.filter(x => x.ok).length;
    console.log(`  ${bs.entries[e]}: ${ep}/${er.length}`);
  }
  if (probes.length) {
    console.log('=== composed_path 探针（前 5 次）===');
    for (const p of probes) {
      console.log(`  target=${p.target} path=${p.path.join(' > ')}`);
    }
  }
  if (failed.length) {
    for (const f of failed) console.log('  - ' + JSON.stringify(f));
    process.exit(1);
  }
})();