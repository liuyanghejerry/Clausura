# Clausura + Playwright 端到端 CI 实践

> 适用版本: clausura 1.0.0
> 仓库: https://github.com/clausura/clausura

## 场景

假设你维护一个 Node.js 电商项目 `mini-shop`。团队开了一个 PR，新增了登录和结账功能。PR 合并前需要两道关卡：Playwright 保证页面功能正常，Clausura 保证代码没有安全问题。

项目文件结构：

```
mini-shop/
  package.json
  server.js                  # Express 后端
  .clausura.yaml             # Clausura 代码审查配置
  .github/
    workflows/
      e2e.yml                # CI 工作流
  public/
    login.html               # 登录页面
    checkout.html            # 结账页面
  tests/
    checkout.spec.ts         # Playwright 浏览器测试
```

目标：PR 合并到 `main` 之前，Playwright 必须通过（检查页面功能），Clausura 也必须通过（检查代码质量）。两者任一失败就阻断合并。

## 项目代码

### package.json

```json
{
  "name": "mini-shop",
  "private": true,
  "scripts": {
    "start": "node server.js",
    "test": "playwright test"
  },
  "dependencies": {
    "express": "^4.18.2"
  },
  "devDependencies": {
    "@playwright/test": "^1.40.0"
  }
}
```

### server.js

一个有意包含漏洞的 Express 服务器。登录接口用 `req.query.user` 直接拼 SQL，存在 SQL 注入漏洞。结账接口正常。

```javascript
const express = require('express');
const app = express();
app.use(express.static('public'));
app.use(express.json());

// 数据库模拟
const db = { query: (sql, callback) => {
  // 模拟查库
  if (sql.includes('admin')) callback(null, [{ name: 'admin', role: 'admin' }]);
  else callback(null, []);
}};

// 有漏洞的登录接口 (SQL 注入)
app.get('/api/login', (req, res) => {
  const user = req.query.user;  // 未做参数化
  const sql = `SELECT * FROM users WHERE name = '${user}'`;  // SQL 注入漏洞
  db.query(sql, (err, rows) => {
    if (rows && rows.length > 0) {
      res.json({ success: true, user: rows[0] });
    } else {
      res.json({ success: false });
    }
  });
});

// 正常的结账接口
app.post('/api/checkout', (req, res) => {
  const { items } = req.body;
  if (!items || items.length === 0) {
    return res.status(400).json({ error: 'Cart is empty' });
  }
  res.json({ success: true, order_id: Date.now() });
});

app.listen(3000, () => console.log('Server on :3000'));
```

### public/login.html

```html
<!DOCTYPE html>
<html>
<head><title>Login</title></head>
<body>
  <h1>Login</h1>
  <input id="username" type="text" placeholder="Username" />
  <button id="login-btn">Login</button>
  <div id="welcome"></div>
  <script>
    document.getElementById('login-btn').onclick = async () => {
      const user = document.getElementById('username').value;
      const res = await fetch(`/api/login?user=${user}`);
      const data = await res.json();
      if (data.success) {
        document.getElementById('welcome').textContent =
          'Welcome, ' + data.user.name;
      }
    };
  </script>
</body>
</html>
```

### public/checkout.html

```html
<!DOCTYPE html>
<html>
<head><title>Checkout</title></head>
<body>
  <h1>Checkout</h1>
  <ul id="items">
    <li>T-shirt - $19.99</li>
    <li>Mug - $9.99</li>
  </ul>
  <button id="checkout-btn">Place Order</button>
  <div id="result"></div>
  <script>
    document.getElementById('checkout-btn').onclick = async () => {
      const res = await fetch('/api/checkout', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ items: ['T-shirt', 'Mug'] })
      });
      const data = await res.json();
      if (data.success) {
        document.getElementById('result').textContent =
          'Order placed: ' + data.order_id;
      } else {
        document.getElementById('result').textContent =
          'Error: ' + data.error;
      }
    };
  </script>
</body>
</html>
```

## Playwright 测试

`tests/checkout.spec.ts` 是一个端到端浏览器测试。它启动浏览器，模拟用户完成登录和结账操作，验证页面是否正确渲染。

```typescript
import { test, expect } from '@playwright/test';

test('checkout flow works end to end', async ({ page }) => {
  await page.goto('http://localhost:3000/login.html');
  await page.fill('#username', 'admin');
  await page.click('#login-btn');
  await expect(page.locator('#welcome')).toContainText('admin');

  await page.goto('http://localhost:3000/checkout.html');
  await page.click('#checkout-btn');
  await expect(page.locator('#result')).toContainText('success');
});
```

这个测试覆盖了完整的用户路径：登录 -> 看到欢迎信息 -> 进入结账页 -> 下单 -> 看到成功提示。如果任何一个步骤的 DOM 发生变化或者接口返回异常，测试就会失败。

## Clausura 配置

`.clausura.yaml` 配置了代码安全审查任务。Clausura 会读取 PR 的代码 diff，交给 LLM 检查安全问题，然后用确定性规则引擎判定是否放行。

```yaml
version: "1"
task:
  name: code-security-review
  model: gpt-4o
  vendor: openai
  prompt_template: |
    Review the following Node.js server code for security issues.
    Focus on:
    1. SQL injection — any string concatenation in SQL queries
    2. Hardcoded credentials or secrets
    3. Missing input validation
    4. XSS vulnerabilities

    For each finding use:
    - rule_id: "sql-injection" for SQL injection, "hardcoded-secret" for credentials, "missing-validation" for input issues, "xss" for XSS
    - severity: "error" for exploitable issues, "warning" for best practice violations
  token_budget: 8000
  timeout_secs: 60
  gating:
    - rule: sql-injection
      description: "SQL injection is a critical security vulnerability"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: hardcoded-secret
      description: "No hardcoded credentials in source"
      min_severity: error
      max_findings: 0
      action: fail
    - rule: missing-validation
      description: "All user inputs must be validated"
      min_severity: warning
      max_findings: 2
      action: warn
```

配置要点：

- `gating` 下定义了三条规则，rule id 需要和 LLM 输出的 `rule_id` 对应
- `sql-injection` 是零容忍，发现一个就 `fail`，阻断 CI
- `hardcoded-secret` 也是零容忍
- `missing-validation` 允许最多 2 个 warning，超出只记录不阻断

## GitHub Actions 工作流

`.github/workflows/e2e.yml` 定义了两个并行 job：`playwright` 和 `clausura`。两者都跑完后，任一失败都会让整个 pipeline 变红。

```yaml
name: E2E Pipeline

on:
  pull_request:
    branches: [main]

jobs:
  playwright:
    name: Playwright Browser Tests
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: actions/setup-node@v4
        with:
          node-version: 20
          cache: 'npm'

      - run: npm ci

      - run: npx playwright install --with-deps chromium

      - name: Start server and run tests
        run: |
          node server.js &
          sleep 2
          npx playwright test

      - uses: actions/upload-artifact@v4
        if: failure()
        with:
          name: playwright-report
          path: playwright-report/

  clausura:
    name: Clausura Code Review
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 2

      - name: Run Clausura
        run: clausura run --config .clausura.yaml
        env:
          CLAUSURA_API_KEY: ${{ secrets.OPENAI_API_KEY }}

      - uses: github/codeql-action/upload-sarif@v3
        if: always()
        with:
          sarif_file: clausura-output.sarif
```

Playwright job 做了三件事：安装依赖、启动服务器、跑浏览器测试。失败时上传测试报告方便排查。Clausura job 需要 `fetch-depth: 2` 来获取 git diff 信息。它把 SARIF 结果上传到 GitHub Advanced Security，让问题直接显示在 PR diff 上。

## 执行结果对比

### 场景 A：两个都通过

| Job | 状态 | 输出 |
|-----|------|------|
| Playwright | ✅ Pass | 3 tests passed, checkout flow works |
| Clausura | ✅ Pass | Findings: 0, Exit: 0 |

PR 可以合并。代码没有安全问题，页面功能正常。

### 场景 B：只有 Clausura 发现 SQL 注入

| Job | 状态 | 输出 |
|-----|------|------|
| Playwright | ✅ Pass | 3 tests passed |
| Clausura | ❌ Fail | Findings: 1 (sql-injection), Exit: 1 |

Playwright 通过了，因为 SQL 注入不影响页面渲染。无论传什么用户名，服务器都返回 JSON，页面照样显示欢迎信息。但 Clausura 从代码层面发现了拼接 SQL 的问题。这正是两种测试互补的体现。

Clausura 的实际日志输出：

```
[1/4] Loading configuration...
[2/4] Initializing agent...
[3/4] Executing task...
  code-security-review
[4/4] Processing results...
Error: Task failed
  Findings: 1 | Exit: 1 | Tokens: 2340 | Duration: 5500ms
```

生成的 SARIF finding 内容：

```json
{
  "ruleId": "sql-injection",
  "level": "error",
  "message": {
    "text": "SQL injection vulnerability in /api/login: user input 'user' is directly concatenated into SQL query without parameterization"
  },
  "locations": [{
    "physicalLocation": {
      "artifactLocation": { "uri": "server.js" },
      "region": { "startLine": 15, "endLine": 16 }
    }
  }]
}
```

这个 SARIF 文件被上传到 GitHub Advanced Security 后，会在 PR 的 `server.js` 第 15-16 行旁边直接标注 "SQL injection vulnerability"，开发者不用看 CI 日志就能发现问题。

### 场景 C：只有 Playwright 发现结账功能挂了

| Job | 状态 | 输出 |
|-----|------|------|
| Playwright | ❌ Fail | checkout flow broken, element not found |
| Clausura | ✅ Pass | Findings: 0, Exit: 0 |

假设有人改了 `checkout.html` 的 DOM 结构，把按钮的 `id` 从 `checkout-btn` 改成了 `submit-order`，但没更新 Playwright 测试。Clausura 检查的是代码层面，看不出 DOM 结构变化。但 Playwright 在浏览器里真实点击，找不到元素就会报错。

这就体现了分层测试的价值：Clausura 管代码质量，Playwright 管功能正确。两者互不替代。

## 关键认识

| 维度 | Playwright | Clausura |
|------|-----------|----------|
| 测试对象 | 用户可见行为（页面渲染、交互） | 代码质量（安全、规范、架构） |
| 失败原因 | 功能回归、DOM 变化 | SQL 注入、硬编码密钥、不良实践 |
| 时机 | 每次 PR | 每次 PR |
| 反馈形式 | 测试报告 + 截图 | SARIF + 门禁判定 |
| 误报率 | 低（真实浏览器执行） | 取决于 LLM 质量和 prompt 设计 |
| 修复成本 | 改测试或改 DOM | 改代码 + 重新审查 |

两种工具各管一摊，加在一起构成完整的质量门禁。Playwright 像质检员，在用户能接触到的地方把关。Clausura 像代码审计师，在开发者写完代码但还没部署之前发现问题。

## 扩展思路

给一个更完整的 pipeline 示意图：

```
PR Push
  ├── Playwright (浏览器测试) --- 3 tests --- ✅ Pass
  ├── Clausura (代码审查)     --- 0 findings - ✅ Pass
  ├── ESLint (代码规范)       --- 0 errors --- ✅ Pass
  └── Build (构建检查)        --- success ---- ✅ Pass
                                    |
                            All green -> Auto-merge
```

你可以在 Playwright 和 Clausura 之外再加入 lint、类型检查、构建验证等步骤。每个步骤都是独立的 job，并行执行，互不阻塞。只有全部通过才允许合并。

Clausura 的配置可以按团队需求定制。换一个 `prompt_template` 和 `gating` 规则，就能从安全检查变成架构合规检查或依赖一致性检查。CI 配置不用动，改 `.clausura.yaml` 就可以。
