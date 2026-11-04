import { chromium } from '/tmp/kprobe/node_modules/playwright-core/index.mjs';

const targetUrl = process.argv[2] || 'https://live.kuaishou.com/u/3xpscnxnjcai83q';
const cookieHeader = process.env.LIVE_BROWSER_COOKIE || '';
const waitMs = Number(process.env.LIVE_BROWSER_WAIT_MS || '15000');
const proxyServer = process.env.LIVE_BROWSER_PROXY || '';
const disableProxy = ['1', 'true', 'yes', 'on'].includes(
  String(process.env.LIVE_BROWSER_NO_PROXY || '').trim().toLowerCase()
);

const interesting = [
  'rest/k/live/byUser',
  'live_api/liveroom/livedetail',
  'm_graphql',
  'startPlay',
  'websocketinfo',
  'gdfp.gifshow.com',
  'verification.kuaishouzt.cn',
  'live_api/baseuser/userinfo',
  'live_api/baseuser/userLogout',
];

function isInteresting(url) {
  return interesting.some((part) => url.includes(part));
}

function isKuaishouUrl(url) {
  return /kuaishou|yximgs|ksapisrv|kskwai/i.test(url);
}

function parseCookieHeader(header) {
  return header
    .split(';')
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => {
      const idx = part.indexOf('=');
      if (idx === -1) {
        return null;
      }
      return {
        name: part.slice(0, idx).trim(),
        value: part.slice(idx + 1).trim(),
      };
    })
    .filter((item) => item && item.name && item.value);
}

function buildCookies(header) {
  const parsed = parseCookieHeader(header);
  const urls = [
    'https://live.kuaishou.com',
    'https://www.kuaishou.com',
    'https://livev.m.chenzhongtech.com',
  ];
  const cookies = [];
  for (const cookie of parsed) {
    for (const url of urls) {
      cookies.push({
        ...cookie,
        url,
        sameSite: 'Lax',
      });
    }
  }
  return cookies;
}

function preview(text, max = 500) {
  const normalized = String(text || '').replace(/\s+/g, ' ').trim();
  return normalized.length > max ? `${normalized.slice(0, max)}...` : normalized;
}

function maybeFullBody(url, text) {
  if (url.includes('gdfp.gifshow.com')) {
    return String(text || '');
  }
  return preview(text || '');
}

const launchOptions = {
  executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
};

if (proxyServer.trim()) {
  launchOptions.proxy = { server: proxyServer.trim() };
}

if (disableProxy) {
  launchOptions.args = ['--no-proxy-server'];
}

const browser = await chromium.launch(launchOptions);

const context = await browser.newContext({
  ignoreHTTPSErrors: true,
  viewport: { width: 1440, height: 900 },
});

if (cookieHeader.trim()) {
  await context.addCookies(buildCookies(cookieHeader));
}

const page = await context.newPage();

page.on('request', async (request) => {
  const url = request.url();
  if (!isInteresting(url)) {
    return;
  }
  const headers = await request.allHeaders();
  console.log(
    JSON.stringify({
      type: 'request',
      url,
      method: request.method(),
      headers: {
        referer: headers.referer,
        origin: headers.origin,
        cookie: headers.cookie,
        'user-agent': headers['user-agent'],
        'content-type': headers['content-type'],
        accept: headers.accept,
        'x-requested-with': headers['x-requested-with'],
        kww: headers.kww,
      },
      postData: maybeFullBody(url, request.postData() || ''),
    })
  );
});

page.on('response', async (response) => {
  const url = response.url();
  if (!isInteresting(url)) {
    return;
  }
  const headers = await response.allHeaders();
  let bodyPreview = '';
  try {
    bodyPreview = maybeFullBody(url, await response.text());
  } catch (error) {
    bodyPreview = `<<unavailable: ${error}>>`;
  }
  console.log(
    JSON.stringify({
      type: 'response',
      url,
      status: response.status(),
      headers: {
        'content-type': headers['content-type'],
        location: headers.location,
        'set-cookie': headers['set-cookie'],
      },
      bodyPreview,
    })
  );
});

page.on('console', (msg) => {
  console.log(JSON.stringify({ type: 'console', text: msg.text() }));
});

page.on('framenavigated', (frame) => {
  const url = frame.url();
  if (!isKuaishouUrl(url)) {
    return;
  }
  console.log(
    JSON.stringify({
      type: 'frame',
      name: frame.name(),
      url,
    })
  );
});

page.on('requestfailed', (request) => {
  const url = request.url();
  if (!isInteresting(url) && !isKuaishouUrl(url)) {
    return;
  }
  console.log(
    JSON.stringify({
      type: 'requestfailed',
      url,
      method: request.method(),
      errorText: request.failure()?.errorText || '',
    })
  );
});

await page.goto(targetUrl, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(waitMs);

const frameInfo = page
  .frames()
  .filter((frame) => isKuaishouUrl(frame.url()))
  .map((frame) => ({ name: frame.name(), url: frame.url() }));
console.log(JSON.stringify({ type: 'frames_snapshot', frames: frameInfo }));

const scriptInfo = await page.evaluate(() =>
  Array.from(document.scripts)
    .map((script) => script.src)
    .filter(Boolean)
);
console.log(JSON.stringify({ type: 'scripts_snapshot', scripts: scriptInfo }));

await context.close();
await browser.close();
