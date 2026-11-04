<script lang="ts">
  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import { invoke } from "../invoker";

  interface Candidate {
    url: string;
    kind: string;
    host: string;
    seen_at_ms: number;
  }
  interface Status {
    supported: boolean;
    ca_installed: boolean;
    running: boolean;
    remaining_secs: number;
    replaced_existing_proxy: boolean;
    candidates: Candidate[];
    hosts: string[];
  }

  const dispatch = createEventDispatcher<{ select: string }>();
  let status: Status | null = null;
  let busy = false;
  let error = "";
  let timer: ReturnType<typeof setInterval> | null = null;
  let finishedWithoutResult = false;

  async function refresh() {
    try {
      const wasRunning = status?.running;
      status = (await invoke("wechat_capture_status")) as Status;
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
    await run("wechat_capture_stop");
    dispatch(
      "select",
      JSON.stringify({ url: candidate.url, title: "视频号直播", captured_stream: true }),
    );
  }

  function clock(seconds: number) {
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
  }

  onMount(refresh);
  onDestroy(() => {
    if (timer) clearInterval(timer);
    // Leaving this dialog must never leave the system proxy pointing at us.
    if (status?.running) invoke("wechat_capture_stop").catch(() => {});
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
      {#if status.candidates.length > 0}
        <ul class="space-y-1">
          {#each status.candidates as candidate (candidate.url)}
            <li class="flex items-center justify-between gap-2 rounded-md bg-white dark:bg-[#2c2c2e] px-2 py-1.5">
              <span class="text-xs truncate" title={candidate.host}>{candidate.kind.toUpperCase()} · {candidate.host}</span>
              <button class="shrink-0 px-2 py-1 text-xs rounded-md bg-blue-600 text-white disabled:opacity-50" disabled={busy}
                on:click={() => choose(candidate)}>使用这个</button>
            </li>
          {/each}
        </ul>
      {:else}
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

    {#if error}
      <p class="text-xs text-red-600 dark:text-red-500">{error}</p>
    {/if}
  </div>
{/if}
