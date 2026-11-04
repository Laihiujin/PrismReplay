<script lang="ts">
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import { invoke } from "../invoker";

  interface Candidate {
    url: string;
    kind: string;
    host: string;
    seen_at_ms: number;
    room_name: string | null;
    room_title: string | null;
  }
  interface Status {
    supported: boolean;
    ca_installed: boolean;
    running: boolean;
    remaining_secs: number;
    replaced_existing_proxy: boolean;
    candidates: Candidate[];
    hosts: string[];
    json_endpoints: string[];
  }

  const dispatch = createEventDispatcher<{ select: string }>();
  let status: Status | null = null;
  let busy = false;
  let error = "";
  let timer: ReturnType<typeof setInterval> | null = null;
  let finishedWithoutResult = false;
  // A failed preview is remembered so that a dead stream is not retried on every refresh.
  let previews: Record<string, string> = {};
  let typedNames: Record<string, string> = {};

  interface Monitor {
    running: boolean;
    host: string;
    interval_mins: number;
    message: string;
    last_check_ms: number;
    last_change_ms: number;
    live_state: string;
    auto_record: boolean;
  }
  let monitor: Monitor | null = null;
  let monitorHost = "";
  let monitorMinutes = 5;
  let monitorAutoRecord = false;
  let monitorTimer: ReturnType<typeof setInterval> | null = null;

  async function refreshMonitor() {
    try {
      monitor = (await invoke("wechat_monitor_status")) as Monitor;
      if (monitor.host && !monitorHost) monitorHost = monitor.host;
    } catch (e) {
      monitor = null;
    }
  }

  async function startMonitor() {
    error = "";
    try {
      await invoke("wechat_monitor_start", { host: monitorHost, intervalMins: Number(monitorMinutes), autoRecord: monitorAutoRecord });
    } catch (e) {
      error = String(e);
    }
    await refreshMonitor();
  }

  async function stopMonitor() {
    try {
      await invoke("wechat_monitor_stop");
    } catch (e) {
      error = String(e);
    }
    await refreshMonitor();
  }

  function loadPreviews(candidates: Candidate[]) {
    for (const candidate of candidates) {
      if (candidate.url in previews) continue;
      previews[candidate.url] = "";
      invoke("wechat_capture_preview", { url: candidate.url })
        .then((image) => (previews[candidate.url] = image as string))
        .catch(() => (previews[candidate.url] = "failed"));
    }
  }

  function streamLabel(candidate: Candidate) {
    if (candidate.room_name) return candidate.room_name;
    const id = candidate.url.match(/live_(\d+)/);
    return id ? `直播 …${id[1].slice(-6)}` : candidate.host;
  }

  function quality(candidate: Candidate) {
    const q = candidate.url.match(/[?&]q=(\d+)/);
    return q ? ` · q=${q[1]}` : "";
  }

  async function refresh() {
    try {
      const wasRunning = status?.running;
      status = (await invoke("wechat_capture_status")) as Status;
      loadPreviews(status.candidates);
      if (status.running && !timer) {
        timer = setInterval(refresh, 1500);
      } else if (!status.running && timer) {
        clearInterval(timer);
        timer = null;
      }
      finishedWithoutResult =
        !status.running && status.candidates.length === 0 && (wasRunning || finishedWithoutResult);
    } catch (e) {
      // Not available outside the desktop app.
      status = null;
    }
  }

  async function run(command: string) {
    busy = true;
    error = "";
    try {
      await invoke(command);
    } catch (e) {
      error = String(e);
    }
    busy = false;
    await refresh();
  }

  async function choose(candidate: Candidate) {
    const name = (typedNames[candidate.url] ?? "").trim() || candidate.room_name || undefined;
    await run("wechat_capture_stop");
    dispatch(
      "select",
      JSON.stringify({
        url: candidate.url,
        title: candidate.room_title || name || "视频号直播",
        name,
        cover: previews[candidate.url]?.startsWith("data:") ? previews[candidate.url] : undefined,
        captured_stream: true,
      }),
    );
  }

  function clock(seconds: number) {
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
  }

  onMount(() => {
    refresh();
    refreshMonitor();
    monitorTimer = setInterval(() => {
      refreshMonitor();
      // Monitoring starts captures on its own, so keep the capture state in view.
      if (monitor?.running) refresh();
    }, 3000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
    if (monitorTimer) clearInterval(monitorTimer);
    // Leaving this dialog must never leave the system proxy pointing at us.
    // Monitoring keeps running in the background and manages its own capture.
    if (status?.running && !monitor?.running) invoke("wechat_capture_stop").catch(() => {});
  });
</script>

{#if status?.supported}
  <div class="rounded-lg bg-[#f5f5f7] dark:bg-[#1c1c1e] p-3 space-y-2 text-sm text-gray-700 dark:text-gray-300">
    <div class="font-medium text-gray-900 dark:text-white">自动抓取直播地址</div>

    {#if !status.ca_installed}
      <p class="text-xs">
        需要先安装本机生成的证书。它只能用于腾讯相关域名，其他网站的流量不会被解密。点击后 Windows 会弹出确认，请选“是”。
      </p>
      <button class="px-3 py-1.5 text-xs rounded-md bg-blue-600 text-white disabled:opacity-50" disabled={busy}
        on:click={() => run("wechat_capture_install_ca")}>安装证书</button>
    {:else if !status.running}
      <p class="text-xs">开始后会临时接管系统代理，停止或退出软件时自动恢复。最长运行 10 分钟。</p>
      {#if finishedWithoutResult}
        <p class="text-xs text-amber-600 dark:text-amber-400">
          这次没有抓到直播地址。请确认是在抓取开始后才进入直播间；如果已在直播间，退出后重新进入再试。
        </p>
      {/if}
      <div class="flex gap-2">
        <button class="px-3 py-1.5 text-xs rounded-md bg-blue-600 text-white disabled:opacity-50" disabled={busy}
          on:click={() => run("wechat_capture_start")}>开始抓取</button>
        <button class="px-3 py-1.5 text-xs rounded-md text-gray-600 dark:text-gray-400 hover:underline disabled:opacity-50" disabled={busy}
          on:click={() => run("wechat_capture_remove_ca")}>移除证书</button>
      </div>
    {:else}
      <p class="text-xs">
        抓取中（剩余 {clock(status.remaining_secs)}）。现在到<strong>电脑版微信</strong>打开要录的视频号直播；如果已经在直播间，请退出后重新进入。
      </p>
      {#if status.replaced_existing_proxy}
        <p class="text-xs text-amber-600 dark:text-amber-400">你原来开着系统代理，抓取期间其他软件不会经过它，结束后自动恢复。</p>
      {/if}
      {#if status.candidates.length === 0}
        <p class="text-xs text-gray-500 dark:text-gray-400">还没有发现直播流…</p>
        {#if status.hosts.length > 0}
          <details class="text-xs text-gray-500 dark:text-gray-400">
            <summary class="cursor-pointer">已经过代理的域名（{status.hosts.length}）</summary>
            <p class="mt-1 break-all">{status.hosts.join("  ")}</p>
          </details>
        {/if}
      {/if}
      <button class="px-3 py-1.5 text-xs rounded-md text-gray-600 dark:text-gray-400 hover:underline disabled:opacity-50" disabled={busy}
        on:click={() => run("wechat_capture_stop")}>停止抓取</button>
    {/if}

    {#if status.candidates.length > 0}
      <ul class="space-y-1 max-h-72 overflow-y-auto">
        {#each status.candidates as candidate (candidate.url)}
          <li class="flex items-center gap-2 rounded-md bg-white dark:bg-[#2c2c2e] p-2">
            <div class="shrink-0 w-14 h-24 rounded bg-black/20 flex items-center justify-center overflow-hidden">
              {#if previews[candidate.url] && previews[candidate.url] !== "failed"}
                <img src={previews[candidate.url]} alt="直播画面" class="w-full h-full object-cover" />
              {:else}
                <span class="text-[10px] text-gray-500">{previews[candidate.url] === "failed" ? "无画面" : "加载中"}</span>
              {/if}
            </div>
            <div class="min-w-0 flex-1 text-xs">
              <div class="truncate font-medium text-gray-900 dark:text-white" title={streamLabel(candidate)}>{streamLabel(candidate)}</div>
              {#if candidate.room_title}
                <div class="truncate" title={candidate.room_title}>{candidate.room_title}</div>
              {/if}
              <div class="truncate text-gray-500 dark:text-gray-400" title={candidate.host}>{candidate.kind.toUpperCase()}{quality(candidate)} · {candidate.host}</div>
              <input
                class="mt-1 w-full rounded bg-[#f5f5f7] dark:bg-[#1c1c1e] px-1.5 py-0.5 text-xs text-gray-900 dark:text-white placeholder-gray-500"
                placeholder={candidate.room_name ? `名字：${candidate.room_name}` : "直播间名字（可选）"}
                bind:value={typedNames[candidate.url]}
              />
            </div>
            <button class="shrink-0 px-2 py-1 text-xs rounded-md bg-blue-600 text-white disabled:opacity-50" disabled={busy}
              on:click={() => choose(candidate)}>使用这个</button>
          </li>
        {/each}
      </ul>
      {#if status.candidates.some((candidate) => !candidate.room_name) && status.json_endpoints.length > 0}
        <details class="text-xs text-gray-500 dark:text-gray-400">
          <summary class="cursor-pointer">没有名字？已读取的接口（{status.json_endpoints.length}）</summary>
          <p class="mt-1 break-all">{status.json_endpoints.join("  ")}</p>
        </details>
      {/if}
    {/if}

      <div class="border-t border-gray-200 dark:border-gray-700 pt-2 space-y-1.5 text-xs">
        <div class="font-medium text-gray-900 dark:text-white">跟踪指定视频号</div>
        <p>
          在电脑版微信搜索目标账号的完整显示名，例如“陈赫”而不是“陈赫工作室”。保留搜索标签，监控会尝试自动选中它；可能短暂切换微信标签。默认只检查状态，不抓包、不改系统代理、不安装证书。页面读不到或同名结果不明确时显示“无法确认”，不会当作未开播。
        </p>
        <div class="flex items-center gap-2">
          <input class="min-w-0 flex-1 rounded bg-white dark:bg-[#2c2c2e] px-2 py-1 text-gray-900 dark:text-white"
            placeholder="目标完整显示名，如 陈赫" bind:value={monitorHost} disabled={monitor?.running} />
          <input class="w-14 rounded bg-white dark:bg-[#2c2c2e] px-2 py-1 text-gray-900 dark:text-white"
            type="number" min="5" title="检查间隔（分钟），最少 5 分钟" bind:value={monitorMinutes} disabled={monitor?.running} />
          <span>分钟</span>
          {#if monitor?.running}
            <button class="shrink-0 px-3 py-1 rounded-md text-gray-600 dark:text-gray-400 hover:underline" on:click={stopMonitor}>停止监控</button>
          {:else}
            <button class="shrink-0 px-3 py-1 rounded-md bg-blue-600 text-white disabled:opacity-50" disabled={!monitorHost.trim()} on:click={startMonitor}>开始监控</button>
          {/if}
        </div>
        <label class="flex items-center gap-2">
          <input type="checkbox" bind:checked={monitorAutoRecord} disabled={monitor?.running || !status.ca_installed} />
          开播后自动抓流录制（可选，需您先安装抓取证书）
        </label>
        <p class="text-gray-500 dark:text-gray-400">仅按完整显示名匹配，不是永久账号 ID；同名账号需人工核对。检查间隔至少 5 分钟，软件和微信需保持运行。</p>
        {#if monitor?.message}
          <p class="text-gray-500 dark:text-gray-400">
            {monitor.host}：{monitor.message}{monitor.last_check_ms ? `（${new Date(monitor.last_check_ms).toLocaleTimeString()}）` : ""}
          </p>
        {/if}
      </div>

    {#if error}
      <p class="text-xs text-red-600 dark:text-red-500">{error}</p>
    {/if}
  </div>
{/if}
