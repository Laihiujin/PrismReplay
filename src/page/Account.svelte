<script lang="ts">
  import { get, invoke, TAURI_ENV } from "../lib/invoker";
  import { scale, fade } from "svelte/transition";
  import { Textarea } from "flowbite-svelte";
  import QRCode from "qrcode";
  import { listen } from "@tauri-apps/api/event";
  import { onDestroy } from "svelte";
  import type { AccountItem, AccountInfo } from "../lib/db";
  import { Ellipsis, Plus, Check } from "lucide-svelte";

  let account_info: AccountInfo = {
    accounts: [],
  };

  let avatar_cache: Map<string, string> = new Map();

  async function update_accounts() {
    let new_account_info = (await invoke("get_accounts")) as AccountInfo;
    for (const account of new_account_info.accounts) {
      if (account.avatar === "") {
        account.avatar = platform_avatar(account.platform);
        continue;
      }
      if (avatar_cache.has(account.avatar)) {
        account.avatar = avatar_cache.get(account.avatar);
        continue;
      }
      const avatar_response = await get(account.avatar);
      const avatar_blob = await avatar_response.blob();
      const avatar_url = URL.createObjectURL(avatar_blob);
      avatar_cache.set(account.avatar, avatar_url);
      account.avatar = avatar_url;
    }
    account_info = new_account_info;
    await update_config();
  }

  async function update_config() {
    try {
      const cfg = await invoke("get_config");
      use_guest_accounts = !!(cfg as any)?.use_guest_accounts;
    } catch {
      use_guest_accounts = false;
    }
  }

  update_accounts();
  update_config();

  let unlisten_accounts = null;
  if (TAURI_ENV) {
    listen("accounts-updated", async () => {
      await update_accounts();
    }).then((unlisten) => {
      unlisten_accounts = unlisten;
    });
  }

  onDestroy(() => {
    cancel_douyin_qr();
    if (unlisten_accounts) {
      unlisten_accounts();
      unlisten_accounts = null;
    }
  });

  let addModal = false;
  let activeTab = "qr"; // 'qr' | 'manual'
  let selectedPlatform = "bilibili";
  let oauth_key = "";
  let check_interval = null;
  let check_interval_ms = 2000;
  let tiktok_backoff = 0;
  let tiktok_rate_timer = null;
  let tiktok_start_timer = null;
  let qr_attempts = 0;
  let qr_max_attempts = 15;
  let auto_qr_poll = true;
  let last_qr_check_ts = 0;
  let min_qr_interval_ms = 3000;
  let cookie_str = "";
  let qr_image = "";
  let qr_url = "";
  let qr_error = "";
  let qr_notice = "";
  let qr_status_inflight = false;
  let webview_cookie_error = "";
  let webview_cookie_loading = false;
  let webview_cookie_polling = false;
  let webview_cookie_poll_timer = null;
  let webview_cookie_poll_attempts = 0;
  let webview_cookie_extra = "";
  let use_guest_accounts = false;
  let guest_refreshing = false;

  let manualModal = false;

  let activeDropdown = null;

  const qrPlatforms = new Set(["bilibili", "douyin", "kuaishou", "tiktok"]);
  const autoQrPlatforms = new Set(["bilibili", "kuaishou"]);
  const tiktok_interval_ms = () => 60000 + Math.floor(Math.random() * 30001);
  const webviewLoginPlatforms: Record<
    string,
    { open: string; get: string; label: string }
  > = {
    tiktok: {
      open: "open_tiktok_login_window",
      get: "get_tiktok_webview_cookies",
      label: "TikTok",
    },
    douyin: {
      open: "open_douyin_login_window",
      get: "get_douyin_webview_cookies",
      label: "抖音",
    },
    kuaishou: {
      open: "open_kuaishou_login_window",
      get: "get_kuaishou_webview_cookies",
      label: "快手",
    },
    huya: {
      open: "open_huya_login_window",
      get: "get_huya_webview_cookies",
      label: "虎牙",
    },
    bilibili: {
      open: "open_bilibili_login_window",
      get: "get_bilibili_webview_cookies",
      label: "B站",
    },
  };

  // Douyin's QR login runs in a hidden app webview, which the web build doesn't have.
  const uses_douyin_webview_qr = (platform: string) =>
    TAURI_ENV && platform === "douyin";

  function default_tab(platform: string) {
    return autoQrPlatforms.has(platform) || uses_douyin_webview_qr(platform)
      ? "qr"
      : "manual";
  }

  function cancel_douyin_qr() {
    if (!oauth_key.startsWith("webview:")) {
      return;
    }
    oauth_key = "";
    if (check_interval) {
      clearInterval(check_interval);
    }
    invoke("cancel_douyin_qr_login").catch((e) => console.log(e));
  }

  $: if (
    !addModal ||
    activeTab !== "qr" ||
    !uses_douyin_webview_qr(selectedPlatform)
  ) {
    cancel_douyin_qr();
  }

  function set_platform(platform: string) {
    selectedPlatform = platform;
    activeTab = default_tab(platform);
    webview_cookie_error = "";
    stop_webview_cookie_poll();
    if (activeTab === "qr" && supports_qr(platform)) {
      requestAnimationFrame(handle_qr);
    }
  }

  function supports_qr(platform: string) {
    return qrPlatforms.has(platform);
  }

  function qr_help_text(platform: string) {
    const helpMap = {
      bilibili: "请使用BiliBili App 扫描二维码登录",
      douyin: "请使用抖音App 扫描二维码登录",
      kuaishou: "请使用快手App 扫描二维码登录",
      tiktok: "请使用TikTok App 扫描二维码登录"
    };
    return helpMap[platform] || "请使用App 扫描二维码登录";
  }
function toggleDropdown(uid) {
    if (activeDropdown === uid) {
      activeDropdown = null;
    } else {
      activeDropdown = uid;
    }
  }

  // Close dropdown when clicking outside
  function handleClickOutside(event) {
    if (
      activeDropdown !== null &&
      !event.target.closest(".dropdown-container")
    ) {
      activeDropdown = null;
    }
  }

  function handleModalClickOutside(event) {
    const modal = document.querySelector(".mac-modal");
    if (
      modal &&
      !modal.contains(event.target) &&
      !event.target.closest("button")
    ) {
      addModal = false;
      stop_webview_cookie_poll();
      if (check_interval) {
        clearInterval(check_interval);
      }
    }
  }

  async function handle_qr() {
    if (check_interval) {
      clearInterval(check_interval);
    }
    if (tiktok_rate_timer) {
      clearTimeout(tiktok_rate_timer);
      tiktok_rate_timer = null;
    }
    if (tiktok_start_timer) {
      clearTimeout(tiktok_start_timer);
      tiktok_start_timer = null;
    }
    tiktok_backoff = 0;
    qr_attempts = 0;
    const douyin_webview = uses_douyin_webview_qr(selectedPlatform);
    qr_max_attempts = douyin_webview ? 150 : selectedPlatform === "tiktok" ? 30 : 15;
    min_qr_interval_ms = douyin_webview ? 1500 : selectedPlatform === "tiktok" ? 60000 : selectedPlatform === "bilibili" ? 5000 : 3000;
    check_interval_ms = selectedPlatform === "tiktok" ? tiktok_interval_ms() : 2000;
    qr_error = "";
    qr_notice = "";
    qr_image = "";
    qr_url = "";
    oauth_key = "";
    const requested_platform = selectedPlatform;
    try {
      let qr_info: { url?: string; image?: string; oauthKey: string } = await invoke(
        "get_qr",
        { platform: requested_platform }
      );
      // The modal may have moved on while the login page was loading.
      if (
        uses_douyin_webview_qr(requested_platform) &&
        (!addModal || activeTab !== "qr" || selectedPlatform !== requested_platform)
      ) {
        invoke("cancel_douyin_qr_login").catch((e) => console.log(e));
        return;
      }
      oauth_key = qr_info.oauthKey;
      qr_image = qr_info.image || "";
      qr_url = qr_info.url || "";

      // The built-in browser is already logged in, so there is nothing to scan.
      if (!qr_image && oauth_key.startsWith("webview:")) {
        qr_notice = "内置浏览器已登录，正在导入账号";
        check_qr_once();
        check_interval = setInterval(check_qr, check_interval_ms);
        return;
      }

      if (qr_image) {
        if (auto_qr_poll) {
          if (selectedPlatform === "tiktok") {
            tiktok_start_timer = setTimeout(() => {
              if (addModal && activeTab === "qr" && supports_qr(selectedPlatform)) {
                check_qr_once();
                check_interval = setInterval(check_qr, check_interval_ms);
              }
            }, check_interval_ms);
          } else {
            check_qr_once();
            check_interval = setInterval(check_qr, check_interval_ms);
          }
        }
        return;
      }

      if (!qr_url) {
        qr_error = "二维码获取失败";
        return;
      }

      const canvas = document.getElementById("qr");
      QRCode.toCanvas(canvas, qr_url, function (error) {
        if (error) {
          console.log(error);
          qr_error = "二维码渲染失败";
          return;
        }
        canvas.style.display = "block";
        if (auto_qr_poll) {
          if (selectedPlatform === "tiktok") {
            tiktok_start_timer = setTimeout(() => {
              if (addModal && activeTab === "qr" && supports_qr(selectedPlatform)) {
                check_qr_once();
                check_interval = setInterval(check_qr, check_interval_ms);
              }
            }, check_interval_ms);
          } else {
            check_qr_once();
            check_interval = setInterval(check_qr, check_interval_ms);
          }
        }
      });
    } catch (e) {
      qr_error = String(e || "二维码获取失败");
    }
  }

  async function check_qr(force = false) {
    if (!auto_qr_poll && !force) {
      return;
    }
    if (!force && selectedPlatform === "tiktok" && tiktok_rate_timer) {
      return;
    }
    const now = Date.now();
    if (!force && now - last_qr_check_ts < min_qr_interval_ms) {
      return;
    }
    if (qr_status_inflight) {
      return;
    }
    qr_status_inflight = true;
    last_qr_check_ts = now;
    const polled_key = oauth_key;
    try {
      qr_attempts += 1;
      if (qr_attempts > qr_max_attempts) {
        if (check_interval) {
          clearInterval(check_interval);
        }
        qr_error = "已暂停轮询，请点击刷新二维码";
        return;
      }
      let qr_status: { code: number; cookies: string; message?: string; extra?: string; image?: string } = await invoke(
        "get_qr_status",
        { platform: selectedPlatform, qrcodeKey: oauth_key }
      );
      if (polled_key !== oauth_key) {
        return;
      }
      if (polled_key.startsWith("webview:") && qr_status.code != 0) {
        if (qr_status.image) {
          qr_image = qr_status.image;
        }
        if (qr_status.code == 2) {
          clearInterval(check_interval);
          qr_notice = "";
          qr_error = qr_status.message || "二维码登录已终止";
        } else {
          qr_error = "";
          qr_notice = qr_status.message === "new" ? "" : qr_status.message || "";
        }
        return;
      }
      if (selectedPlatform === "kuaishou" && qr_status.cookies?.trim()) {
        const parts = oauth_key.split("|");
        if (parts.length >= 2) {
          oauth_key = `${parts[0]}|${parts[1]}|${qr_status.cookies.trim()}`;
        }
      }
      if (qr_status.code == 0) {
        clearInterval(check_interval);
        await invoke("update_login_account", {
          cookies: qr_status.cookies,
          platform: selectedPlatform,
          extra: qr_status.extra || webview_cookie_extra || undefined,
        });
        await update_accounts();
        addModal = false;
        return;
      }
      if (qr_status.code == 1 || qr_status.code == 2) {
        if (qr_status.message && qr_status.message !== "new") {
          qr_error = qr_status.message;
          if (qr_status.message.includes("访问太频繁")) {
            if (check_interval) {
              clearInterval(check_interval);
            }
            tiktok_backoff += 1;
            check_interval_ms = tiktok_interval_ms();
            if (selectedPlatform === "tiktok") {
              if (tiktok_rate_timer) {
                clearTimeout(tiktok_rate_timer);
              }
              const cooldown_ms = 300000 + Math.floor(Math.random() * 300001);
              tiktok_rate_timer = setTimeout(() => {
                tiktok_rate_timer = null;
                if (addModal && activeTab === "qr" && supports_qr(selectedPlatform) && auto_qr_poll) {
                  check_qr_once();
                  check_interval = setInterval(check_qr, check_interval_ms);
                }
              }, cooldown_ms);
            } else {
              if (auto_qr_poll) {
                check_interval = setInterval(check_qr, check_interval_ms);
              }
            }
          }
        }
        return;
      }
      if (qr_status.code == 2) {
        if (check_interval) {
          clearInterval(check_interval);
        }
        qr_error = qr_status.message || "二维码登录已终止";
        return;
      }
      if (selectedPlatform !== "bilibili") {
        qr_error = qr_status.message || "扫码未确认";
      } else if (qr_status.message && qr_status.message !== "new") {
        qr_error = qr_status.message;
      } else {
        qr_error = "";
      }
    } catch (e) {
      qr_error = String(e || "二维码状态获取失败");
    } finally {
      qr_status_inflight = false;
    }
  }

  async function check_qr_once() {
    if (!supports_qr(selectedPlatform) || !oauth_key) {
      return;
    }
    await check_qr(true);
  }

  async function add_cookie() {
    if (cookie_str == "") {
      return;
    }
    try {
      console.log("add_cookie", selectedPlatform);
      await invoke("add_account", {
        cookies: cookie_str,
        platform: selectedPlatform,
        extra: webview_cookie_extra || undefined,
      });
      if (webview_cookie_extra) {
        await invoke("update_login_account", {
          cookies: cookie_str,
          platform: selectedPlatform,
          extra: webview_cookie_extra || undefined,
        });
      }
      await update_accounts();
      cookie_str = "";
      webview_cookie_extra = "";
      addModal = false;
    } catch (e) {
      alert("添加账号失败：" + e);
    }
  }

  function supports_webview_login(platform: string) {
    return TAURI_ENV && !!webviewLoginPlatforms[platform];
  }

  async function open_webview_login() {
    webview_cookie_error = "";
    const config = webviewLoginPlatforms[selectedPlatform];
    if (!config) {
      return;
    }
    try {
      await invoke(config.open, {
        userAgent: navigator.userAgent,
      });
    } catch (e) {
      webview_cookie_error = String(e || "打开登录窗口失败");
    }
  }

  async function import_webview_cookies() {
    return import_webview_cookies_internal(false);
  }

  async function import_webview_cookies_internal(silent: boolean) {
    webview_cookie_error = "";
    webview_cookie_loading = true;
    const config = webviewLoginPlatforms[selectedPlatform];
    if (!config) {
      webview_cookie_loading = false;
      return "";
    }
    try {
      const result = await invoke(config.get);
      const cookies =
        typeof result === "string" ? result : (result as any)?.cookies || "";
      webview_cookie_extra =
        typeof result === "string" ? "" : (result as any)?.extra || "";
      cookie_str = cookies;
      return cookies;
    } catch (e) {
      if (!silent) {
        webview_cookie_error = String(e || "导入 Cookie 失败");
      }
    } finally {
      webview_cookie_loading = false;
    }
    return "";
  }

  function has_tiktok_login_cookie(cookies: string) {
    if (!cookies) {
      return false;
    }
    const lower = cookies.toLowerCase();
    return (
      lower.includes("sessionid=") ||
      lower.includes("sid_tt=") ||
      lower.includes("sid_guard=") ||
      lower.includes("uid_tt=") ||
      lower.includes("passport_csrf_token=")
    );
  }

  function stop_webview_cookie_poll() {
    if (webview_cookie_poll_timer) {
      clearInterval(webview_cookie_poll_timer);
      webview_cookie_poll_timer = null;
    }
    webview_cookie_polling = false;
    webview_cookie_poll_attempts = 0;
  }

  function start_webview_cookie_poll() {
    if (webview_cookie_polling) {
      return;
    }
    webview_cookie_polling = true;
    webview_cookie_poll_attempts = 0;
    const max_attempts = 90;
    const poll_once = async () => {
      webview_cookie_poll_attempts += 1;
      const cookies = await import_webview_cookies_internal(true);
      if (has_tiktok_login_cookie(cookies)) {
        stop_webview_cookie_poll();
        await add_cookie();
        return;
      }
      if (webview_cookie_poll_attempts >= max_attempts) {
        stop_webview_cookie_poll();
        webview_cookie_error = "未检测到登录态 Cookie，请确认已登录 TikTok";
      }
    };
    poll_once();
    webview_cookie_poll_timer = setInterval(poll_once, 2000);
  }

  function platform_display(platform: string) {
    const platformMap = {
      bilibili: "B站",
      douyin: "抖音",
      huya: "虎牙",
      kuaishou: "快手",
      tiktok: "TikTok"
    };
    return platformMap[platform] || platform;
  }
function platform_avatar(platform: string) {
    const avatarMap = {
      bilibili: "/imgs/bilibili.svg",
      douyin: "/imgs/Tiktok_avatar.svg",
      huya: "/imgs/huya.png",
      kuaishou: "/imgs/kuaishou.svg",
      tiktok: "/imgs/TikTok-Wordmark.svg"
    };
    return avatarMap[platform] || "/imgs/bili.svg";
  }

  // 关闭当前平台的登录窗口
  async function close_current_login_window() {
    const config = webviewLoginPlatforms[selectedPlatform];
    if (!config) {
      return;
    }
    const label = `${selectedPlatform}-login`;
    try {
      await invoke("close_webview_window", { label });
      webview_cookie_error = "";
    } catch (e) {
      // 窗口可能已经关闭，忽略错误
      console.log("关闭窗口:", e);
    }
  }

  // 关闭所有登录窗口
  async function close_all_windows() {
    try {
      const closedWindows = await invoke("close_all_login_windows");
      console.log("已关闭窗口:", closedWindows);
      webview_cookie_error = "";
    } catch (e) {
      console.error("关闭窗口失败:", e);
    }
  }

  async function force_refresh_guest_cookies() {
    if (!use_guest_accounts || guest_refreshing) {
      return;
    }
    guest_refreshing = true;
    try {
      await invoke("refresh_guest_accounts_force");
      await update_accounts();
    } catch (e) {
      alert("强制刷新访客 Cookie 失败：" + e);
    } finally {
      guest_refreshing = false;
    }
  }

</script>

<svelte:window
  on:click={handleClickOutside}
  on:mousedown={handleModalClickOutside}
/>

<div class="flex-1 p-6 overflow-auto custom-scrollbar-light bg-gray-50 dark:bg-black">
  <div class="space-y-6">
    <!-- Header -->
    <div class="flex justify-between items-center">
      <div class="flex items-center space-x-4">
        <h1 class="text-2xl font-semibold text-gray-900 dark:text-white">账号</h1>

        <div
          class="flex items-center space-x-2 text-sm text-gray-500 dark:text-gray-400"
        >
          <span>共 {account_info.accounts.length} 个</span>
        </div>
      </div>
      <div class="flex items-center space-x-3">
        {#if use_guest_accounts}
          <button
            on:click={force_refresh_guest_cookies}
            disabled={guest_refreshing}
            class="px-4 py-2 rounded-lg border-2 border-yellow-400 text-yellow-400 hover:bg-yellow-400/10 transition-colors disabled:opacity-60 disabled:cursor-not-allowed"
          >
            {guest_refreshing ? "刷新中..." : "强制刷新"}
          </button>
        {/if}
        <button
          on:click={() => {
            addModal = true;
            activeTab = default_tab(selectedPlatform);
            if (activeTab === "qr" && supports_qr(selectedPlatform)) {
              requestAnimationFrame(handle_qr);
            }
          }}
          class="px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600 transition-colors flex items-center space-x-2"
        >
          <Plus class="w-5 h-5 icon-white" />
          <span>添加账号</span>
        </button>
      </div>
    </div>

    <!-- Account List -->
    <div class="space-y-4">
      <!-- Online Account -->
      {#each account_info.accounts as account (account.uid)}
        <div
          class="p-4 rounded-xl bg-white dark:bg-[#3c3c3e] border border-gray-200 dark:border-gray-700 hover:border-blue-500 dark:hover:border-blue-400 transition-colors"
        >
          <div class="flex items-center justify-between">
            <div class="flex items-center space-x-4">
              <div class="relative shrink-0">
                <img
                  alt="avatar"
                  class="w-12 h-12 rounded-full object-cover"
                  src={account.avatar}
                />
              </div>
              <div>
                <div class="flex items-center space-x-2">
                  <span
                    class="inline-flex items-center px-2 py-1 text-xs font-medium rounded-full {account.platform ===
                    'bilibili'
                      ? 'bg-pink-100 text-pink-800 dark:bg-pink-900 dark:text-pink-200'
                    : account.platform === 'douyin' || account.platform === 'tiktok'
                      ? 'bg-black text-white'
                        : account.platform === 'huya'
                          ? 'text-white'
                          : 'bg-gray-100 text-gray-800 dark:bg-gray-700 dark:text-gray-200'}"
                    style={account.platform === "huya"
                      ? "background-color: #ff9600"
                      : ""}
                  >
                    {platform_display(account.platform)}
                  </span>
                  <h3 class="font-medium text-gray-900 dark:text-white">
                    {account.name || account.uid}
                  </h3>
                </div>
                <p class="text-sm text-gray-600 dark:text-gray-400">
                  UID: {account.uid}
                </p>
              </div>
            </div>
            <div class="flex items-center space-x-3">
              <div class="relative dropdown-container">
                <button
                  class="p-2 rounded-lg hover:bg-[#e5e5e5] dark:hover:bg-[#3a3a3c]"
                  on:click|stopPropagation={() => toggleDropdown(account.uid)}
                >
                  <Ellipsis class="w-5 h-5 dark:icon-white" />
                </button>
                {#if activeDropdown === account.uid}
                  <div
                    class="absolute right-0 mt-2 w-48 rounded-lg shadow-lg bg-white dark:bg-[#3c3c3e] border border-gray-200 dark:border-gray-700 backdrop-blur-xl bg-opacity-90 dark:bg-opacity-90"
                    style="transform-origin: top right;"
                    in:scale={{ duration: 100, start: 0.95 }}
                    out:scale={{ duration: 100, start: 0.95 }}
                  >

                    <button
                      class="w-full px-4 py-2 text-left text-sm text-red-600 hover:bg-[#e5e5e5] dark:hover:bg-[#3a3a3c] rounded-lg"
                      on:click={async () => {
                        await invoke("remove_account", {
                          platform: account.platform,
                          uid: account.uid,
                        });
                        await update_accounts();
                        activeDropdown = null;
                      }}
                    >
                      注销账号
                    </button>
                  </div>
                {/if}
              </div>
            </div>
          </div>
        </div>
      {/each}

      <!-- Add Account Card -->
      <button
        class="w-full p-4 rounded-xl border-2 border-dashed border-gray-300 dark:border-gray-600 hover:border-blue-500 dark:hover:border-blue-400 transition-colors"
        on:click={() => {
          addModal = true;
          activeTab = default_tab(selectedPlatform);
          if (activeTab === "qr" && supports_qr(selectedPlatform)) {
            requestAnimationFrame(handle_qr);
          }
        }}
      >
        <div class="flex flex-col items-center justify-center space-y-2">
          <div
            class="w-12 h-12 rounded-full bg-blue-500/10 flex items-center justify-center"
          >
            <Plus class="w-6 h-6 icon-primary" />
          </div>
          <div class="text-center">
            <p class="text-sm font-medium text-blue-600 dark:text-blue-400">
              添加新账号
            </p>
            <p class="text-xs text-gray-500 dark:text-gray-400">
              添加一个新账号，用于获取直播流和投稿
            </p>
          </div>
        </div>
      </button>
    </div>
  </div>
</div>

{#if addModal}
  <div
    class="fixed inset-0 bg-black/20 dark:bg-black/40 backdrop-blur-sm z-50 flex items-center justify-center"
    transition:fade={{ duration: 200 }}
  >
    <div
      class="mac-modal w-[400px] bg-white dark:bg-[#323234] rounded-xl shadow-xl overflow-hidden"
      transition:scale={{ duration: 150, start: 0.95 }}
    >
      <!-- Header -->
      <div class="px-6 py-4 border-b border-gray-200 dark:border-gray-700/50">
        <h2 class="text-base font-medium text-gray-900 dark:text-white">
          添加账号
        </h2>
      </div>

      <div class="p-6 space-y-6">
        <!-- Platform Selection -->
        <div class="space-y-2">
          <label
            for="platform"
            class="block text-sm font-medium text-gray-700 dark:text-gray-300"
          >
            平台
          </label>
          <div class="grid grid-cols-5 gap-2 p-0.5 bg-[#f5f5f7] dark:bg-[#1c1c1e] rounded-lg">
            <button
              class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
              'bilibili'
                ? 'bg-white dark:bg-[#3c3c3e] shadow-sm text-gray-900 dark:text-white'
                : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
              on:click={() => set_platform("bilibili")}
            >
              bilibili
            </button>
            <button
              class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
              'douyin'
                ? 'bg-white dark:bg-[#3c3c3e] shadow-sm text-gray-900 dark:text-white'
                : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
              on:click={() => set_platform("douyin")}
            >
              抖音
            </button>
            <button
              class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
              'huya'
                ? 'bg-white dark:bg-[#3c3c3e] shadow-sm text-gray-900 dark:text-white'
                : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
              on:click={() => set_platform("huya")}
            >
              虎牙
            </button>
            <button
              class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
              'kuaishou'
                ? 'bg-white dark:bg-[#3c3c3e] shadow-sm text-gray-900 dark:text-white'
                : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
              on:click={() => set_platform("kuaishou")}
            >
              快手
            </button>
            <button
              class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
              'tiktok'
                ? 'bg-white dark:bg-[#3c3c3e] shadow-sm text-gray-900 dark:text-white'
                : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
              on:click={() => set_platform("tiktok")}
            >
              TikTok
            </button>
          </div>
        </div>

        <!-- Login Methods -->
        {#if supports_qr(selectedPlatform)}
          <div class="flex rounded-lg bg-[#f5f5f7] dark:bg-[#1c1c1e] p-1">
            <button
              class="flex-1 px-4 py-1.5 text-sm rounded-md transition-colors {activeTab ===
              'qr'
                ? 'bg-white dark:bg-[#3c3c3e] shadow-sm font-medium'
                : 'text-gray-600 dark:text-gray-400'}"
              on:click={() => {
                activeTab = "qr";
                requestAnimationFrame(handle_qr);
              }}
            >
              二维码登录
            </button>
            <button
              class="flex-1 px-4 py-1.5 text-sm rounded-md transition-colors {activeTab ===
              'manual'
                ? 'bg-white dark:bg-[#3c3c3e] shadow-sm font-medium'
                : 'text-gray-600 dark:text-gray-400'}"
              on:click={() => {
                activeTab = "manual";
              }}
            >
              手动 Cookie
            </button>
          </div>
        {:else}
          <div class="flex rounded-lg bg-[#f5f5f7] dark:bg-[#1c1c1e] p-1">
            <button
              class="flex-1 px-4 py-1.5 text-sm rounded-md transition-colors bg-white dark:bg-[#3c3c3e] shadow-sm font-medium"
              on:click={() => {
                activeTab = "manual";
              }}
            >
              手动 Cookie
            </button>
          </div>
        {/if}

        <!-- Tab Content -->
        <div class="space-y-4">
          {#if activeTab === "qr" && supports_qr(selectedPlatform)}
            <div class="flex flex-col items-center space-y-4">
              <div class="bg-white p-4 rounded-lg">
                {#if qr_image}
                  <img src={qr_image} alt="qr" class="w-56 h-56 object-contain" />
                {:else}
                  <canvas id="qr" />
                {/if}
              </div>
              {#if qr_error}
                <p class="text-sm text-center text-red-500">{qr_error}</p>
              {:else if qr_notice}
                <p class="text-sm text-center text-gray-600 dark:text-gray-400">{qr_notice}</p>
              {/if}
              <button
                class="text-xs text-gray-600 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white transition-colors"
                on:click={handle_qr}
              >
                刷新二维码
              </button>
              <p class="text-sm text-center text-gray-600 dark:text-gray-400">
                {qr_help_text(selectedPlatform)}
              </p>
            </div>
          {:else}
            <div class="space-y-4">
              <p class="text-sm text-gray-600 dark:text-gray-400">
                <Textarea
                  bind:value={cookie_str}
                  rows={4}
                  class="w-full px-3 py-2 bg-[#f5f5f7] dark:bg-[#1c1c1e] border-0 rounded-lg resize-none focus:ring-2 focus:ring-blue-500"
                  placeholder={`请粘贴 ${selectedPlatform} 账号的 Cookie`}
                />
              </p>
              {#if supports_webview_login(selectedPlatform)}
                <div class="space-y-2">
                  <div class="flex items-center justify-between gap-2">
                    <button
                      class="px-3 py-2 bg-[#2c2c2e] hover:bg-[#3a3a3c] text-white text-xs font-medium rounded-lg transition-colors"
                      on:click={open_webview_login}
                    >
                      打开内置浏览器登录
                    </button>
                    <button
                      class="px-3 py-2 bg-[#1f6feb] hover:bg-[#1f6feb]/90 text-white text-xs font-medium rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                      on:click={import_webview_cookies}
                      disabled={webview_cookie_loading}
                    >
                      {webview_cookie_loading ? "导入中..." : "导入 Cookie"}
                    </button>
                  </div>
                  <p class="text-xs text-gray-500 dark:text-gray-400">
                    登录完成后点击导入，Cookie 会自动写入输入框
                  </p>
                  {#if webview_cookie_error}
                    <p class="text-xs text-red-500">{webview_cookie_error}</p>
                  {/if}
                </div>
              {/if}
              <div class="flex justify-end items-center space-x-2">
                <button
                  class="px-4 py-2 bg-red-500 hover:bg-red-600 text-white text-sm font-medium rounded-lg transition-colors"
                  on:click={close_all_windows}
                  title="关闭所有Webview窗口"
                >
                  关闭所有窗口
                </button>
                <button
                  class="px-4 py-2 bg-[#0A84FF] hover:bg-[#0A84FF]/90 text-white text-sm font-medium rounded-lg transition-colors"
                  on:click={() => {
                    add_cookie();
                  }}
                >
                  添加账号
                </button>
              </div>
            </div>
          {/if}
        </div>
      </div>
    </div>
  </div>
{/if}
