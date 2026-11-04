<script lang="ts">
  import Room from "./page/Room.svelte";
  import BSidebar from "./lib/components/BSidebar.svelte";
  import Summary from "./page/Summary.svelte";
  import Setting from "./page/Setting.svelte";
  import Account from "./page/Account.svelte";
  import { log, onOpenUrl } from "./lib/invoker";
  import Clip from "./page/Clip.svelte";
  import Task from "./page/Task.svelte";
  import AI from "./page/AI.svelte";
  import Archive from "./page/Archive.svelte";
  import { onMount } from "svelte";

  const ROUTE = {
    OVERVIEW: "overview",
    ROOM: "room",
    ARCHIVE: "archive",
    CLIP: "clip",
    TASK: "task",
    AI: "ai",
    ACCOUNT: "account",
    SETTING: "setting",
  } as const;

  type RouteKey = (typeof ROUTE)[keyof typeof ROUTE];

  let active: RouteKey = ROUTE.OVERVIEW;
  let darkMode = false;

  function applyTheme(isDark: boolean) {
    darkMode = isDark;
    document.documentElement.classList.toggle("dark", isDark);
  }

  function handleActiveChange(e: CustomEvent<string>) {
    active = (e.detail as RouteKey) || ROUTE.OVERVIEW;
  }

  onMount(async () => {
    try {
      log.info("App mounted");
    } catch (e) {
      console.error("Failed to log mount", e);
    }

    const theme = localStorage.getItem("theme");
    const isDark = theme ? theme === "dark" : true;
    applyTheme(isDark);
    if (!theme) {
      localStorage.setItem("theme", "dark");
    }

    await onOpenUrl((urls: string[]) => {
      console.log("Received Deep Link:", urls);
      if (urls.length === 0) {
        return;
      }

      const url = urls[0].replace(/^prismreplay:///, "bsr://");
      let platform = "";
      let room_id = "";

      const bilibiliPrefixes = [
        "bsr://live.bilibili.com/",
        "https://live.bilibili.com/",
        "http://live.bilibili.com/",
      ];
      for (const prefix of bilibiliPrefixes) {
        if (url.startsWith(prefix)) {
          room_id = url.replace(prefix, "").split("?")[0];
          platform = "bilibili";
          break;
        }
      }

      const douyinPrefixes = [
        "bsr://live.douyin.com/",
        "https://live.douyin.com/",
        "http://live.douyin.com/",
      ];
      for (const prefix of douyinPrefixes) {
        if (url.startsWith(prefix)) {
          room_id = url.replace(prefix, "").split("?")[0];
          platform = "douyin";
          break;
        }
      }

      if (url.startsWith("bsr://live.kuaishou.com/")) {
        room_id = url.replace("bsr://live.kuaishou.com/", "").split("?")[0];
        room_id = room_id.replace(/^u\//, "");
        platform = "kuaishou";
      }

      if (url.startsWith("bsr://live.tiktok.com/")) {
        room_id = url.replace("bsr://live.tiktok.com/", "").split("?")[0];
        platform = "tiktok";
      }

      const webcastMatch = url.match(
        /webcast\.tiktok\.com\/webcast\/[^?]+.*[?&]room_id=(\d+)/i
      );
      if (webcastMatch) {
        room_id = webcastMatch[1];
        platform = "tiktok";
      }

      if (
        url.startsWith("https://www.tiktok.com/") ||
        url.startsWith("http://www.tiktok.com/") ||
        url.startsWith("https://tiktok.com/") ||
        url.startsWith("http://tiktok.com/")
      ) {
        const match = url.match(/tiktok\.com\/@?([^\/\?]+)(?:\/live)?/i);
        if (match) {
          room_id = match[1];
          platform = "tiktok";
        }
      }

      if (!room_id) {
        const huyaMatch = url.match(
          /(?:https?|bsr):\/\/(?:www\.)?huya\.com\/([^?#\s,;，。]+?)(?=https?:\/\/|bsr:\/\/|[,;，。\s]|$)/i
        );
        if (huyaMatch) {
          room_id = huyaMatch[1];
          platform = "huya";
          log.info("Parsed Huya room_id:", room_id);
        }
      }

      if (platform && room_id) {
        active = ROUTE.ROOM;
      }
    });
  });
</script>

<main>
  <div class="wrap">
    <div class="sidebar">
      <BSidebar
        bind:activeUrl={active}
        on:activeChange={handleActiveChange}
      />
    </div>
    <div class="content bg-white dark:bg-black">
      <div class="page" class:visible={active == ROUTE.OVERVIEW}>
        <Summary />
      </div>
      <div class="page" class:visible={active == ROUTE.ROOM}>
        <Room />
      </div>
      <div class="page" class:visible={active == ROUTE.ARCHIVE}>
        <Archive />
      </div>
      <div class="page" class:visible={active == ROUTE.CLIP}>
        <Clip />
      </div>
      <div class="page" class:visible={active == ROUTE.TASK}>
        <Task />
      </div>
      <div class="page" class:visible={active == ROUTE.AI}>
        <AI />
      </div>
      <div class="page" class:visible={active == ROUTE.ACCOUNT}>
        <Account />
      </div>
      <div class="page" class:visible={active == ROUTE.SETTING}>
        <Setting />
      </div>
    </div>
  </div>
</main>

<style>
  .sidebar {
    display: flex;
    height: 100vh;
  }

  .wrap {
    display: flex;
    flex-direction: row;
    height: 100vh;
    overflow: hidden;
    background: #fff;
  }

  :global(.dark) .wrap {
    background: #000;
  }

  .visible {
    opacity: 1 !important;
    height: 100% !important;
    transform: translateX(0) !important;
  }

  .page {
    opacity: 0;
    height: 0;
    transform: translateX(100%);
    overflow: hidden;
    transition:
      opacity 0.5s ease-in-out,
      transform 0.3s ease-in-out;
    display: flex;
    flex-direction: column;
  }

  .content {
    width: 100%;
    height: 100vh;
    overflow: hidden;
  }
</style>
