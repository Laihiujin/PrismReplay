<script lang="ts">
  import { onDestroy, onMount } from "svelte";
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

  /** Adds the captured stream to the room list. Throws with a message when that fails. */
  export let onAdd: (resource: string) => Promise<void>;
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
    account_id: string;
    interval_mins: number;
    message: string;
    last_check_ms: number;
    last_change_ms: number;
    live_state: string;
    auto_record: boolean;
  }
  let monitor: Monitor | null = null;
  let monitorHost = "";
  let monitorAccountId = "";
  let monitorMinutes = 5;
  let monitorAutoRecord = false;
  let monitorTimer: ReturnType<typeof setInterval> | null = null;

  async function refreshMonitor() {
    try {
      monitor = (await invoke("wechat_monitor_status")) as Monitor;
      if (monitor.host && !monitorHost) monitorHost = monitor.host;
      if (monitor.account_id && !monitorAccountId && monitor.host === monitorHost) monitorAccountId = monitor.account_id;
    } catch (e) {
      monitor = null;
    }
  }

  async function bindMonitorProfile() {
    error = "";
    try {
      const target = await invoke("wechat_monitor_bind", { host: monitorHost }) as { host: string; account_id: string };
      monitorHost = target.host;
      monitorAccountId = target.account_id;
    } catch (e) { error = String(e); }
    await refreshMonitor();
  }

  async function startMonitor() {
    error = "";
    try {
      await invoke("wechat_monitor_start", { host: monitorHost, accountId: monitorAccountId, intervalMins: Number(monitorMinutes), autoRecord: monitorAutoRecord });
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

  function streamLabel(candidate: Candidate, names: Record<string, string>) {
    const note = (names[candidate.url] ?? "").trim();
    if (note) return note;
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

  // Streams the user removed from the list, or already added, for this dialog.
  let dismissed: Record<string, true> = {};
  let added: Record<string, true> = {};
  let adding: Record<string, boolean> = {};
  $: visibleCandidates = (status?.candidates ?? []).filter((c) => !dismissed[c.url]);

  // The stream shown large in the preview view, with a frame refreshed every few seconds.
  let previewing: Candidate | null = null;
  let previewFrame = "";
  let previewError = "";
  let previewTimer: ReturnType<typeof setTimeout> | null = null;
  let previewToken = 0;

  function openPreview(candidate: Candidate) {
    previewing = candidate;
    previewFrame = previews[candidate.url]?.startsWith("data:") ? previews[candidate.url] : "";
    previewError = "";
    const token = ++previewToken;
    const tick = async () => {
      try {
        const image = (await invoke("wechat_capture_preview", { url: candidate.url })) as string;
        if (token !== previewToken) return;
        previewFrame = image;
        previews[candidate.url] = image;
        previewError = "";
      } catch (e) {
        if (token !== previewToken) return;
        previewError = String(e);
      }
      if (token === previewToken) previewTimer = setTimeout(tick, 3000);
    };
    tick();
  }

  function closePreview() {
    previewToken++;
    if (previewTimer) clearTimeout(previewTimer);
    previewTimer = null;
    previewing = null;
  }

  function resourceFor(candidate: Candidate) {
    const name = (typedNames[candidate.url] ?? "").trim() || candidate.room_name || undefined;
    return JSON.stringify({
      url: candidate.url,
      title: candidate.room_title || name || "视频号直播",
      name,
      cover: previews[candidate.url]?.startsWith("data:") ? previews[candidate.url] : undefined,
      captured_stream: true,
    });
  }

  async function addToHome(candidate: Candidate) {
    error = "";
    adding[candidate.url] = true;
    try {
      // Keep capturing only while the user may still add more streams; the
      // system proxy is restored once nothing is left to pick from.
      const others = visibleCandidates.filter((c) => c.url !== candidate.url && !added[c.url]);
      if (others.length === 0 && status?.running && !monitor?.running) await run("wechat_capture_stop");
      await onAdd(resourceFor(candidate));
      added[candidate.url] = true;
      if (previewing?.url === candidate.url) closePreview();
    } catch (e) {
      error = String(e);
    }
    adding[candidate.url] = false;
  }

  function dismiss(candidate: Candidate) {
    dismissed[candidate.url] = true;
    if (previewing?.url === candidate.url) closePreview();
  }

  function seenAt(candidate: Candidate) {
    return candidate.seen_at_ms ? new Date(candidate.seen_at_ms).toLocaleTimeString() : "";
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
    closePreview();
    // Leaving this dialog must never leave the system proxy pointing at us.
    // Monitoring keeps running in the background and manages its own capture.
    if (status?.running && !monitor?.running) invoke("wechat_capture_stop").catch(() => {});
  });
</script>

{#if status?.supported}
  <div class="rounded-lg bg-[#f5f5f7] dark:bg-[#1c1c1e] p-3 space-y-2 text-sm text-gray-700 dark:text-gray-300">
   <details class="group space-y-2" open>
    <summary class="cursor-pointer select-none font-medium text-gray-900 dark:text-white">
      自动抓取直播地址{#if visibleCandidates.length > 0}<span class="ml-1 text-xs font-normal text-gray-500">（{visibleCandidates.length} 个）</span>{/if}
    </summary>

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

    {#if previewing}
      <div class="space-y-2 rounded-md bg-white dark:bg-[#2c2c2e] p-2">
        <div class="flex items-center gap-2">
          <button class="px-2 py-1 text-xs rounded-md bg-gray-200 dark:bg-gray-700" on:click={closePreview}>← 返回</button>
          <div class="min-w-0 flex-1 truncate font-medium text-gray-900 dark:text-white" title={streamLabel(previewing, typedNames)}>{streamLabel(previewing, typedNames)}</div>
        </div>
        <div class="w-full aspect-video rounded bg-black flex items-center justify-center overflow-hidden">
          {#if previewFrame}
            <img src={previewFrame} alt="直播画面" class="w-full h-full object-contain" />
          {:else}
            <span class="text-xs text-gray-400">{previewError || "正在读取画面…"}</span>
          {/if}
        </div>
        {#if previewFrame && previewError}
          <p class="text-xs text-amber-600 dark:text-amber-400">画面暂停更新：{previewError}</p>
        {:else}
          <p class="text-xs text-gray-500 dark:text-gray-400">画面每 3 秒刷新一次，无声音。</p>
        {/if}
        <div class="text-xs space-y-0.5">
          {#if previewing.room_title}<div class="break-all">标题：{previewing.room_title}</div>{/if}
          <div class="text-gray-500 dark:text-gray-400 break-all">
            {previewing.kind.toUpperCase()}{quality(previewing)} · {previewing.host}{seenAt(previewing) ? ` · 抓到于 ${seenAt(previewing)}` : ""}
          </div>
        </div>
        <input
          class="w-full rounded bg-[#f5f5f7] dark:bg-[#1c1c1e] px-2 py-1 text-xs text-gray-900 dark:text-white placeholder-gray-500"
          placeholder={previewing.room_name ? `备注：${previewing.room_name}` : "备注（作为直播间名字显示）"}
          bind:value={typedNames[previewing.url]}
        />
        <div class="flex gap-2">
          <button class="flex-1 px-3 py-1.5 text-xs rounded-md bg-blue-600 text-white disabled:opacity-50"
            disabled={busy || adding[previewing.url] || added[previewing.url]}
            on:click={() => previewing && addToHome(previewing)}>
            {added[previewing.url] ? "已添加" : adding[previewing.url] ? "添加中…" : "增加此直播间到主页"}
          </button>
          <button class="px-3 py-1.5 text-xs rounded-md bg-gray-200 dark:bg-gray-700" on:click={closePreview}>返回</button>
        </div>
      </div>
    {:else if visibleCandidates.length > 0}
      <ul class="space-y-1">
        {#each visibleCandidates as candidate (candidate.url)}
          <li class="flex items-center gap-2 rounded-md bg-white dark:bg-[#2c2c2e] p-2">
            <button class="shrink-0 w-14 h-24 rounded bg-black/20 flex items-center justify-center overflow-hidden" title="预览"
              on:click={() => openPreview(candidate)}>
              {#if previews[candidate.url] && previews[candidate.url] !== "failed"}
                <img src={previews[candidate.url]} alt="直播画面" class="w-full h-full object-cover" />
              {:else}
                <span class="text-[10px] text-gray-500">{previews[candidate.url] === "failed" ? "无画面" : "加载中"}</span>
              {/if}
            </button>
            <div class="min-w-0 flex-1 text-xs">
              <div class="truncate font-medium text-gray-900 dark:text-white" title={streamLabel(candidate, typedNames)}>{streamLabel(candidate, typedNames)}</div>
              {#if candidate.room_title}
                <div class="truncate" title={candidate.room_title}>{candidate.room_title}</div>
              {/if}
              <div class="truncate text-gray-500 dark:text-gray-400" title={candidate.host}>{candidate.kind.toUpperCase()}{quality(candidate)} · {candidate.host}</div>
              <input
                class="mt-1 w-full rounded bg-[#f5f5f7] dark:bg-[#1c1c1e] px-1.5 py-0.5 text-xs text-gray-900 dark:text-white placeholder-gray-500"
                placeholder={candidate.room_name ? `备注：${candidate.room_name}` : "备注（作为直播间名字显示）"}
                bind:value={typedNames[candidate.url]}
              />
            </div>
            <div class="shrink-0 flex flex-col gap-1">
              <button class="px-2 py-1 text-xs rounded-md bg-gray-200 dark:bg-gray-700 text-gray-900 dark:text-white"
                on:click={() => openPreview(candidate)}>预览</button>
              <button class="px-2 py-1 text-xs rounded-md bg-blue-600 text-white disabled:opacity-50"
                disabled={busy || adding[candidate.url] || added[candidate.url]}
                on:click={() => addToHome(candidate)}>{added[candidate.url] ? "已添加" : adding[candidate.url] ? "添加中" : "添加"}</button>
              <button class="px-2 py-1 text-xs rounded-md text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20"
                on:click={() => dismiss(candidate)}>删除</button>
            </div>
          </li>
        {/each}
      </ul>
      {#if !previewing && visibleCandidates.some((candidate) => !candidate.room_name) && status.json_endpoints.length > 0}
        <details class="text-xs text-gray-500 dark:text-gray-400">
          <summary class="cursor-pointer">没有名字？已读取的接口（{status.json_endpoints.length}）</summary>
          <p class="mt-1 break-all">{status.json_endpoints.join("  ")}</p>
        </details>
      {/if}
    {/if}

   </details>

      <details class="border-t border-gray-200 dark:border-gray-700 pt-2 space-y-1.5 text-xs" open={monitor?.running}>
        <summary class="cursor-pointer select-none font-medium text-gray-900 dark:text-white">
          跟踪指定视频号{#if monitor?.running}<span class="ml-1 font-normal text-green-600 dark:text-green-400">监控中</span>{/if}
        </summary>
        <p>
          先在电脑版微信打开本人视频号主页（不是搜索结果），填写显示名并点“绑定当前主页”。绑定后按主页账号 ID 检查状态，同名账号不会替代它。默认只监控，不抓包、不改系统代理；微信需保持主页可读，读取失败时显示“无法确认”。
        </p>
        <div class="flex items-center gap-2">
          <input class="min-w-0 flex-1 rounded bg-white dark:bg-[#2c2c2e] px-2 py-1 text-gray-900 dark:text-white"
            placeholder="本人主页显示名，如 陈赫" bind:value={monitorHost} on:input={() => monitorAccountId = ""} disabled={monitor?.running} />
          <button class="shrink-0 px-2 py-1 rounded-md bg-gray-200 dark:bg-gray-700 disabled:opacity-50" disabled={monitor?.running || !monitorHost.trim()} on:click={bindMonitorProfile}>绑定当前主页</button>
          <input class="w-14 rounded bg-white dark:bg-[#2c2c2e] px-2 py-1 text-gray-900 dark:text-white"
            type="number" min="5" title="检查间隔（分钟），最少 5 分钟" bind:value={monitorMinutes} disabled={monitor?.running} />
          <span>分钟</span>
          {#if monitor?.running}
            <button class="shrink-0 px-3 py-1 rounded-md text-gray-600 dark:text-gray-400 hover:underline" on:click={stopMonitor}>停止监控</button>
          {:else}
            <button class="shrink-0 px-3 py-1 rounded-md bg-blue-600 text-white disabled:opacity-50" disabled={!monitorHost.trim() || !monitorAccountId} on:click={startMonitor}>开始监控</button>
          {/if}
        </div>
        <label class="flex items-center gap-2">
          <input type="checkbox" bind:checked={monitorAutoRecord} disabled={monitor?.running || !status.ca_installed} />
          开播后自动抓流录制（可选，需您先安装抓取证书）
        </label>
        <p class="text-gray-500 dark:text-gray-400">每次读取都核对已绑定的 @finder 账号标识，显示名只作为导航提示。不会保存主页里的登录票据或完整带签名地址。检查至少间隔 5 分钟。</p>
        {#if monitorAccountId}<p class="text-gray-500 dark:text-gray-400 break-all">已绑定账号：{monitorAccountId.slice(0, 12)}…{monitorAccountId.slice(-12)}</p>{/if}
        {#if monitor?.message}
          <p class="text-gray-500 dark:text-gray-400">
            {monitor.host}：{monitor.message}{monitor.last_check_ms ? `（${new Date(monitor.last_check_ms).toLocaleTimeString()}）` : ""}
          </p>
        {/if}
      </details>

    {#if error}
      <p class="text-xs text-red-600 dark:text-red-500">{error}</p>
    {/if}
  </div>
{/if}
