<script lang="ts">
  import { invoke, open, onOpenUrl, get_static_url, get } from "../lib/invoker";
  import { message } from "@tauri-apps/plugin-dialog";
  import { fade, scale } from "svelte/transition";
  import type { RecorderList, RecorderInfo } from "../lib/interface";
  import type { RecordItem } from "../lib/db";
  import {
    Play,
    Plus,
    FileVideo,
    Search,
    Trash2,
    X,
    History,
    PlayIcon,
    Globe,
  } from "lucide-svelte";
  import BilibiliIcon from "../lib/components/BilibiliIcon.svelte";
  import DouyinIcon from "../lib/components/DouyinIcon.svelte";
  import KuaishouIcon from "../lib/components/KuaishouIcon.svelte";
  import HuyaIcon from "../lib/components/HuyaIcon.svelte";
  import TikTokIcon from "../lib/components/TikTokIcon.svelte";
  import AutoRecordIcon from "../lib/components/AutoRecordIcon.svelte";
  import GenerateWholeClipModal from "../lib/components/GenerateWholeClipModal.svelte";
  import { onMount, onDestroy } from "svelte";

  export let room_count = 0;
  let room_active = 0;
  let room_inactive = 0;

  let summary: RecorderList = {
    count: 0,
    recorders: [],
  };

  let searchQuery = "";
  $: filteredRecorders = summary.recorders.filter((room) => {
    const query = searchQuery.toLowerCase();
    return (
      room.room_info.room_title.toLowerCase().includes(query) ||
      room.user_info.user_name.toLowerCase().includes(query) ||
      room.room_info.room_id.toString().includes(query)
    );
  });
  let isUpdating = false;
  let multiSelect = false;
  let selectedRoomKeys: string[] = [];
  $: selectedRoomKeySet = new Set(selectedRoomKeys);
  $: selectedRoomCount = selectedRoomKeys.length;
  const cardElements: Map<string, HTMLElement> = new Map();
  let gridRef: HTMLDivElement;
  let dragSelecting = false;
  let dragStart = { x: 0, y: 0 };
  let dragRect: { left: number; top: number; width: number; height: number } | null =
    null;
  let dragMoveRaf = 0;
  let summaryTimer: ReturnType<typeof setInterval> | null = null;
  let reloadPendingKeys = new Set<string>();

  function roomKey(room: RecorderInfo) {
    return `${room.room_info.platform}:${room.room_info.room_id}`;
  }

  function isRoomSelected(room: RecorderInfo) {
    return selectedRoomKeySet.has(roomKey(room));
  }

  function toggleRoomSelection(room: RecorderInfo) {
    const key = roomKey(room);
    if (selectedRoomKeySet.has(key)) {
      selectedRoomKeySet.delete(key);
    } else {
      selectedRoomKeySet.add(key);
    }
    selectedRoomKeys = Array.from(selectedRoomKeySet);
  }

  function clearRoomSelection() {
    selectedRoomKeys = [];
  }

  function selectAllFiltered() {
    selectedRoomKeys = filteredRecorders.map((room) => roomKey(room));
  }

  function getSelectedRooms() {
    const selected = new Map<string, RecorderInfo>();
    for (const room of summary.recorders) {
      const key = roomKey(room);
      if (selectedRoomKeySet.has(key)) {
        selected.set(key, room);
      }
    }
    return Array.from(selected.values());
  }

  function cardRef(node: HTMLElement, room: RecorderInfo) {
    const key = roomKey(room);
    cardElements.set(key, node);
    return {
      update(newRoom: RecorderInfo) {
        const newKey = roomKey(newRoom);
        if (newKey !== key) {
          cardElements.delete(key);
          cardElements.set(newKey, node);
        }
      },
      destroy() {
        cardElements.delete(key);
      },
    };
  }

  function shouldIgnoreSelectionTarget(target: HTMLElement) {
    if (!target) return false;
    return !!target.closest(
      "button, a, input, textarea, select, svg, path, .dropdown, .flowbite-dropdown, [data-ignore-selection]"
    );
  }

  function handleRoomClick(event: MouseEvent, room: RecorderInfo) {
    if (!multiSelect) return;
    const target = event.target as HTMLElement;
    if (shouldIgnoreSelectionTarget(target)) return;
    toggleRoomSelection(room);
  }

  function handleGridMouseDown(event: MouseEvent) {
    if (!multiSelect) return;
    if (event.button !== 0) return;
    const target = event.target as HTMLElement;
    if (shouldIgnoreSelectionTarget(target)) return;
    dragSelecting = true;
    dragStart = { x: event.clientX, y: event.clientY };
    dragRect = { left: event.clientX, top: event.clientY, width: 0, height: 0 };
    event.preventDefault();
  }

  function updateDragRect(x: number, y: number) {
    const left = Math.min(dragStart.x, x);
    const top = Math.min(dragStart.y, y);
    const width = Math.abs(x - dragStart.x);
    const height = Math.abs(y - dragStart.y);
    dragRect = { left, top, width, height };
  }

  function handleDragMove(event: MouseEvent) {
    if (!dragSelecting) return;
    const x = event.clientX;
    const y = event.clientY;
    if (dragMoveRaf) return;
    dragMoveRaf = requestAnimationFrame(() => {
      dragMoveRaf = 0;
      if (!dragSelecting) return;
      updateDragRect(x, y);
    });
  }

  function handleDragEnd() {
    if (dragMoveRaf) {
      cancelAnimationFrame(dragMoveRaf);
      dragMoveRaf = 0;
    }
    if (!dragSelecting) return;
    dragSelecting = false;
    if (!dragRect || (dragRect.width < 4 && dragRect.height < 4)) {
      dragRect = null;
      return;
    }
    const selected: string[] = [];
    for (const [key, el] of cardElements) {
      const rect = el.getBoundingClientRect();
      const intersects =
        rect.left < dragRect.left + dragRect.width &&
        rect.left + rect.width > dragRect.left &&
        rect.top < dragRect.top + dragRect.height &&
        rect.top + rect.height > dragRect.top;
      if (intersects) {
        selected.push(key);
      }
    }
    selectedRoomKeys = selected;
    dragRect = null;
  }

  function default_avatar(platform: string) {
    const avatarMap = {
      bilibili: "/imgs/bilibili_avatar.png",
      douyin: "/imgs/douyin.svg",
      huya: "/imgs/huya_avatar.png",
      kuaishou: "/imgs/kuaishou.svg",
      tiktok: "/imgs/Tiktok.svg",
      wechat: "/imgs/wechat.svg",
    };
    return avatarMap[platform] || "/imgs/huya_avatar.png";
  }

  function default_cover(platform: string) {
    const coverMap = {
      bilibili: "/imgs/bilibili.png",
      douyin: "/imgs/douyin.svg",
      huya: "/imgs/huya.png",
      kuaishou: "/imgs/kuaishou.svg",
      tiktok: "/imgs/Tiktok.svg",
      wechat: "/imgs/wechat.svg",
    };
    return coverMap[platform] || "/imgs/huya.png";
  }

  const avatar_cache: Map<string, string> = new Map();
  const avatar_last_key: Map<string, string> = new Map();
  function build_avatar_cache_key(room_id: string, url: string) {
    return `${room_id}:${url}`;
  }
  function cleanup_avatar_cache(room_id: string, next_key: string) {
    const prev_key = avatar_last_key.get(room_id);
    if (prev_key && prev_key !== next_key) {
      const prev_blob = avatar_cache.get(prev_key);
      if (prev_blob) {
        URL.revokeObjectURL(prev_blob);
      }
      avatar_cache.delete(prev_key);
    }
    avatar_last_key.set(room_id, next_key);
  }
  async function get_avatar_url(room_id: string, url: string) {
    const cache_key = build_avatar_cache_key(room_id, url);
    if (avatar_cache.has(cache_key)) {
      return avatar_cache.get(cache_key);
    }
    const response = await get(url);
    const blob = await response.blob();
    const avatar_url = URL.createObjectURL(blob);
    avatar_cache.set(cache_key, avatar_url);
    return avatar_url;
  }

  const image_cache: Map<string, string> = new Map();
  const cover_last_key: Map<string, string> = new Map();
  function build_cover_cache_key(room_id: string, url: string) {
    return `${room_id}:${url}`;
  }
  function cleanup_cover_cache(room_id: string, next_key: string) {
    const prev_key = cover_last_key.get(room_id);
    if (prev_key && prev_key !== next_key) {
      const prev_blob = image_cache.get(prev_key);
      if (prev_blob) {
        URL.revokeObjectURL(prev_blob);
      }
      image_cache.delete(prev_key);
    }
    cover_last_key.set(room_id, next_key);
  }
  async function get_image_url(room_id: string, url: string) {
    const cache_key = build_cover_cache_key(room_id, url);
    if (image_cache.has(cache_key)) {
      return image_cache.get(cache_key);
    }
    const response = await get(url);
    const blob = await response.blob();
    const cover_url = URL.createObjectURL(blob);
    image_cache.set(cache_key, cover_url);
    return cover_url;
  }

  async function update_summary(force = false) {
    if (isUpdating) return;
    if (!force && (dragSelecting || multiSelect)) return;
    isUpdating = true;
    try {
      let new_summary = (await invoke("get_recorder_list")) as RecorderList;
      room_count = new_summary.count;

      let activeCount = 0;
      for (const room of new_summary.recorders) {
        if (room.room_info.status) activeCount += 1;
      }
      room_active = activeCount;
      room_inactive = new_summary.recorders.length - activeCount;

      // sort new_summary.recorders by live_status
      new_summary.recorders.sort((a, b) => {
        if (a.room_info.status && !b.room_info.status) return -1;
        if (!a.room_info.status && b.room_info.status) return 1;
        return 0;
      });

      const mediaTasks: Promise<void>[] = [];

      for (const room of new_summary.recorders) {
        const roomId = String(room.room_info.room_id);
        if (room.user_info.user_avatar != "") {
          const avatarSource = room.user_info.user_avatar;
          const nextAvatarKey = build_avatar_cache_key(roomId, avatarSource);
          if (avatar_last_key.get(roomId) !== nextAvatarKey) {
            cleanup_avatar_cache(roomId, nextAvatarKey);
          }
          const cachedAvatar = avatar_cache.get(nextAvatarKey);
          if (cachedAvatar) {
            room.user_info.user_avatar = cachedAvatar;
          } else {
            mediaTasks.push(
              (async () => {
                room.user_info.user_avatar = await get_avatar_url(
                  roomId,
                  avatarSource
                );
              })()
            );
          }
        } else {
          room.user_info.user_avatar = default_avatar(room.room_info.platform);
        }

        if (room.room_info.room_cover != "") {
          const coverSource = room.room_info.room_cover;
          const nextCoverKey = build_cover_cache_key(roomId, coverSource);
          if (cover_last_key.get(roomId) !== nextCoverKey) {
            cleanup_cover_cache(roomId, nextCoverKey);
          }
          const cachedCover = image_cache.get(nextCoverKey);
          if (cachedCover) {
            room.room_info.room_cover = cachedCover;
          } else {
            mediaTasks.push(
              (async () => {
                room.room_info.room_cover = await get_image_url(roomId, coverSource);
              })()
            );
          }
        } else if (room.user_info.user_avatar != "") {
          room.room_info.room_cover = room.user_info.user_avatar;
        } else {
          room.room_info.room_cover = default_cover(room.room_info.platform);
        }
      }

      if (mediaTasks.length > 0) {
        await Promise.all(mediaTasks);
      }

      summary = new_summary;
      const validKeys = new Set(new_summary.recorders.map((room) => roomKey(room)));
      selectedRoomKeys = selectedRoomKeys.filter((key) => validKeys.has(key));
    } finally {
      isUpdating = false;
    }
  }
  update_summary();
  summaryTimer = setInterval(update_summary, 5000);

  // modals
  let deleteModal = false;
  let deleteRoom: RecorderInfo = null;
  let deleteRooms: RecorderInfo[] = [];
  let deleteMode: "single" | "bulk" = "single";

  let addModal = false;
  let addRoom = "";
  let addValid = false;
  let addErrorMsg = "";
  let selectedPlatform = "bilibili";
  let batchMode = false;
  let batchInput = "";
  let batchValidCount = 0;
  let batchInvalidCount = 0;
  let batchEntries: { roomId: string; platform: string }[] = [];
  const KUAISHOU_RESERVED_ROOM_IDS = new Set([
    "kuaishou",
    "live",
    "www",
    "u",
    "profile",
    "home",
  ]);
  // generate whole clip modal state
  let generateWholeClipModal = false;
  let generateWholeClipArchive: RecordItem | null = null;
  $: {
    if (!batchMode) {
      const trimmed = addRoom.trim();
      const parsed = parseRoomInput(trimmed);
      const normalizedRoomId =
        parsed.platform === selectedPlatform && parsed.roomId
          ? parsed.roomId
          : trimmed;
      const needsNumeric =
        selectedPlatform === "bilibili" || selectedPlatform === "douyin" || selectedPlatform === "huya";
      const needsAlphanumeric = selectedPlatform === "kuaishou";

      if (selectedPlatform === "wechat") {
        addValid = trimmed.length > 0 && trimmed.length <= 131072 && (/^https?:\/\//i.test(trimmed) || trimmed.startsWith("{"));
        addErrorMsg = trimmed && !addValid ? "请粘贴媒体直链或单条资源 JSON" : "";
      } else if (!normalizedRoomId) {
        addValid = false;
        addErrorMsg = "";
      } else if (needsNumeric && !/^\d+$/.test(normalizedRoomId)) {
        addValid = false;
        addErrorMsg = "ID格式错误，请检查输入（仅限数字）";
      } else if (needsAlphanumeric && !isValidKuaishouRoomId(normalizedRoomId)) {
        addValid = false;
        addErrorMsg = "ID格式错误，请输入快手用户ID（支持字母/数字/-/_）";
      } else if (/[\u4e00-\u9fa5]/.test(normalizedRoomId)) {
        addValid = false;
        addErrorMsg = "ID包含非法字符（请勿输入汉字）";
      } else {
        addValid = true;
        addErrorMsg = "";
      }
      batchValidCount = 0;
      batchInvalidCount = 0;
      batchEntries = [];
    } else {
      const { entries, invalidCount } = buildBatchEntries(
        batchInput,
        selectedPlatform
      );
      batchEntries = entries;
      batchValidCount = entries.length;
      batchInvalidCount = invalidCount;
      addValid = entries.length > 0;
      addErrorMsg = "";
    }
  }

  let archiveModal = false;
  let archiveRoom: RecorderInfo = null;
  let archives: RecordItem[] = [];

  // 分页相关状态
  let currentPage = 0;
  let pageSize = 20;
  let hasMore = true;
  let isLoading = false;
  let loadError = "";

  async function showArchives(room_id: string) {
    // 重置分页状态
    currentPage = 0;
    archives = [];
    hasMore = true;
    isLoading = false;
    loadError = "";

    updateArchives();
    archiveModal = true;
    console.log(archives);
  }

  async function updateArchives() {
    if (isLoading || !hasMore) return;

    isLoading = true;

    try {
      let new_archives = (await invoke("get_archives", {
        roomId: archiveRoom.room_info.room_id,
        offset: currentPage * pageSize,
        limit: pageSize,
      })) as RecordItem[];

      for (const archive of new_archives) {
        archive.cover = await get_static_url(
          "cache",
          `${archive.platform}/${archive.room_id}/${archive.live_id}/cover.jpg`
        );
      }

      // 如果是第一页，直接替换；否则追加数据
      if (currentPage === 0) {
        archives = new_archives;
      } else {
        archives = [...archives, ...new_archives];
      }

      // 检查是否还有更多数据
      hasMore = new_archives.length === pageSize;

      // 按时间排序
      archives.sort((a, b) => {
        return (
          new Date(b.created_at).getTime() - new Date(a.created_at).getTime()
        );
      });

      currentPage++;
    } catch (error) {
      console.error("Failed to load archives:", error);
      loadError = "加载失败，请重试";
    } finally {
      isLoading = false;
    }
  }

  async function loadMoreArchives() {
    await updateArchives();
  }

  function handleScroll(event: Event) {
    const target = event.target as HTMLElement;
    const { scrollTop, scrollHeight, clientHeight } = target;

    // 当滚动到距离底部100px时，自动加载更多
    if (
      scrollHeight - scrollTop - clientHeight < 100 &&
      hasMore &&
      !isLoading
    ) {
      loadMoreArchives();
    }
  }

  function format_ts(ts_string: string) {
    const date = new Date(ts_string);
    return date.toLocaleString();
  }

  function format_duration(duration: number) {
    duration = Math.round(duration);
    const hours = Math.floor(duration / 3600)
      .toString()
      .padStart(2, "0");
    const minutes = Math.floor((duration % 3600) / 60)
      .toString()
      .padStart(2, "0");
    const seconds = (duration % 60).toString().padStart(2, "0");

    return `${hours}:${minutes}:${seconds}`;
  }

  function format_size(size: number) {
    if (size < 1024) {
      return `${size} B`;
    } else if (size < 1024 * 1024) {
      return `${(size / 1024).toFixed(2)} KiB`;
    } else if (size < 1024 * 1024 * 1024) {
      return `${(size / 1024 / 1024).toFixed(2)} MiB`;
    } else {
      return `${(size / 1024 / 1024 / 1024).toFixed(2)} GiB`;
    }
  }

  function calc_bitrate(size: number, duration: number) {
    return ((size * 8) / duration / 1024).toFixed(0);
  }

  function handleModalClickOutside(event: MouseEvent) {
    const clickedElement = event.target as HTMLElement;

    if (clickedElement.closest("button")) {
      return;
    }

    if (generateWholeClipModal) {
      return;
    }

    if (archiveModal) {
      const archiveModalEl = document.querySelector(".archive-modal");
      if (archiveModalEl) {
        if (archiveModalEl.contains(clickedElement)) {
          return;
        } else {
          archiveModal = false;
          return;
        }
      }
    }

    if (addModal) {
      const addModalEl = document.querySelector(".add-modal");
      if (addModalEl) {
        if (addModalEl.contains(clickedElement)) {
          return;
        } else {
          addModal = false;
          return;
        }
      }
    }

    if (deleteModal) {
      const deleteModalEl = document.querySelector(".delete-modal");
      if (deleteModalEl) {
        if (deleteModalEl.contains(clickedElement)) {
          return;
        } else {
          deleteModal = false;
          return;
        }
      }
    }
  }

  async function toggleEnabled(room: RecorderInfo) {
    const nextEnabled = !room.enabled;
    room.enabled = nextEnabled;
    summary = { ...summary, recorders: [...summary.recorders] };
    try {
      await invoke("set_enable", {
        roomId: room.room_info.room_id,
        platform: room.room_info.platform,
        enabled: nextEnabled,
      });
      await update_summary(true);
    } catch (error) {
      room.enabled = !nextEnabled;
      summary = { ...summary, recorders: [...summary.recorders] };
      console.error("Failed to toggle room enable state:", error);
      await message("切换直播间开关失败", { title: "操作失败", kind: "error" });
    }
  }

  function openUserUrl(room: RecorderInfo) {
    if (room.room_info.platform === "bilibili") {
      open("https://space.bilibili.com/" + room.user_info.user_id);
    } else if (room.room_info.platform === "douyin") {
      if (room.user_info.user_id) {
        open("https://www.douyin.com/user/" + room.user_info.user_id);
      } else {
        openLiveUrl(room);
      }
    } else if (room.room_info.platform === "kuaishou") {
      if (room.user_info.user_id) {
        open("https://live.kuaishou.com/profile/" + room.user_info.user_id);
      } else {
        openLiveUrl(room);
      }
    } else if (room.room_info.platform === "tiktok") {
      if (room.user_info.user_id) {
        const handle = room.user_info.user_id.startsWith("@")
          ? room.user_info.user_id
          : `@${room.user_info.user_id}`;
        open(`https://www.tiktok.com/${handle}`);
      } else {
        openLiveUrl(room);
      }
    }
  }

  function openLiveUrl(room: RecorderInfo) {
    if (room.room_info.room_id.startsWith("http")) {
      open(room.room_info.room_id);
      return;
    }

    if (room.room_info.platform === "bilibili") {
      open("https://live.bilibili.com/" + room.room_info.room_id);
    } else if (room.room_info.platform === "douyin") {
      open("https://live.douyin.com/" + room.room_info.room_id);
    } else if (room.room_info.platform === "kuaishou") {
      open("https://live.kuaishou.com/u/" + room.room_info.room_id);
    } else if (room.room_info.platform === "tiktok") {
      const handle = room.room_info.room_id.startsWith("@")
        ? room.room_info.room_id
        : `@${room.room_info.room_id}`;
      open(`https://www.tiktok.com/${handle}/live`);
    } else if (room.room_info.platform === "wechat") {
      open("https://channels.weixin.qq.com/");
    } else if (room.room_info.platform === "huya") {
      open("https://www.huya.com/" + room.room_info.room_id);
    }
  }

  async function reloadRoom(room: RecorderInfo) {
    const key = roomKey(room);
    if (reloadPendingKeys.has(key)) {
      return;
    }
    reloadPendingKeys.add(key);
    reloadPendingKeys = new Set(reloadPendingKeys);
    try {
      await invoke("reload_recorder", {
        roomId: room.room_info.room_id,
        platform: room.room_info.platform,
      });
      await update_summary();
    } catch (e) {
      console.error("Failed to reload recorder:", e);
      await message(String(e), {
        title: "重载失败",
        kind: "warning",
      });
    } finally {
      reloadPendingKeys.delete(key);
      reloadPendingKeys = new Set(reloadPendingKeys);
    }
  }

  function openLivePreview(room: RecorderInfo) {
    if (room.recording && room.live_id) {
      invoke("open_live", {
        platform: room.room_info.platform,
        roomId: room.room_info.room_id,
        liveId: room.live_id,
      });
      return;
    }
    openLiveUrl(room);
  }

  function parseRoomInput(raw: string) {
    const trimmed = raw.trim();
    if (!trimmed) {
      return { roomId: "", platform: "" };
    }

    const patterns = [
      {
        platform: "bilibili",
        re: new RegExp(
          String.raw`(?:bsr://)?https?://live\.bilibili\.com/(\d+)`,
          "i"
        ),
      },
      {
        platform: "bilibili",
        re: new RegExp(String.raw`bsr://live\.bilibili\.com/(\d+)`, "i"),
      },
      {
        platform: "douyin",
        re: new RegExp(
          String.raw`(?:bsr://)?https?://live\.douyin\.com/(\d+)`,
          "i"
        ),
      },
      {
        platform: "kuaishou",
        re: new RegExp(
          String.raw`(?:bsr://)?https?://(?:live|www)\.kuaishou\.com/(?:u|profile)/([A-Za-z0-9_-]+)/?(?:\?.*)?`,
          "i"
        ),
      },
      {
        platform: "tiktok",
        re: new RegExp(
          String.raw`webcast\.tiktok\.com/webcast/(?:feed|room/info|im/fetch)/.*[?&]room_id=(\d+)`,
          "i"
        ),
      },
      {
        platform: "tiktok",
        re: new RegExp(
          String.raw`(?:bsr://)?https?://(?:www\.)?tiktok\.com/@?([^/\?]+)/live`,
          "i"
        ),
      },
      {
        platform: "tiktok",
        re: new RegExp(
          String.raw`(?:bsr://)?https?://(?:www\.)?tiktok\.com/@?([^/\?]+)/?$`,
          "i"
        ),
      },
      // 虎牙直播间URL - 必须匹配到URL结尾
      {
        platform: "huya",
        re: new RegExp(
          String.raw`(?:bsr://)?https?://(?:www\.)?huya\.com/(\d+)/?(?:\?.*)?`,
          "i"
        ),
      },
      {
        platform: "huya",
        re: new RegExp(
          String.raw`(?:bsr://)?https?://m\.huya\.com/(\d+)/?(?:\?.*)?`,
          "i"
        ),
      },
    ];

    for (const { platform, re } of patterns) {
      const match = trimmed.match(re);
      if (match) {
        return { roomId: match[1], platform };
      }
    }

    return { roomId: "", platform: "" };
  }

  function sanitizeKuaishouRoomId(input: string) {
    return input
      .trim()
      .trimStart()
      .replace(/^@/, "")
      .replace(/^\/+|\/+$/g, "")
      .replace(/\.html$/i, "")
      .trim();
  }

  function isValidKuaishouRoomId(input: string) {
    const roomId = sanitizeKuaishouRoomId(input);
    if (!roomId) return false;
    if (!/^[A-Za-z0-9_-]+$/.test(roomId)) return false;
    return !KUAISHOU_RESERVED_ROOM_IDS.has(roomId.toLowerCase());
  }

  function normalizeRoomInput(raw: string, platform: string) {
    if (platform === "wechat") return { roomId: raw.trim(), platform };
    const parsed = parseRoomInput(raw);
    const trimmed = raw.trim();
    const parsedRoomId =
      parsed.platform === "kuaishou"
        ? sanitizeKuaishouRoomId(parsed.roomId)
        : parsed.roomId;
    if (parsedRoomId && (!platform || parsed.platform === platform)) {
      return { roomId: parsedRoomId, platform: parsed.platform || platform };
    }
    if (platform === "kuaishou") {
      return { roomId: sanitizeKuaishouRoomId(trimmed), platform };
    }
    return { roomId: trimmed, platform };
  }

  function normalizeBatchInput(raw: string, defaultPlatform: string) {
    const parsed = parseRoomInput(raw);
    const trimmed = raw.trim();
    const parsedRoomId =
      parsed.platform === "kuaishou"
        ? sanitizeKuaishouRoomId(parsed.roomId)
        : parsed.roomId;
    if (parsedRoomId) {
      return { roomId: parsedRoomId, platform: parsed.platform || defaultPlatform };
    }
    if (defaultPlatform === "kuaishou") {
      return { roomId: sanitizeKuaishouRoomId(trimmed), platform: defaultPlatform };
    }
    return { roomId: trimmed, platform: defaultPlatform };
  }

  function buildBatchEntries(raw: string, defaultPlatform: string) {
    // 先将转义的 \n 替换为真实换行符，再按行分割
    const normalizedRaw = raw.replace(/\\n/g, "\n").replace(/\\r/g, "");
    // 同时支持 换行、逗号、分号、空格、中文逗号、中文分号 作为分隔符
    const lines = normalizedRaw.split(/[\n\r,; \t，；]+/);
    const entries: { roomId: string; platform: string }[] = [];
    const seen = new Set<string>();
    let invalidCount = 0;

    for (const line of lines) {
      const trimmed = line.trim().replace(/[，,；; \t]+$/, "").replace(/^[，,；; \t]+/, "");
      if (!trimmed) continue;
      const normalized = normalizeBatchInput(trimmed, defaultPlatform);
      if (!normalized.roomId) {
        invalidCount += 1;
        continue;
      }
      const needsNumeric =
        normalized.platform === "bilibili" || normalized.platform === "douyin" || normalized.platform === "huya";
      const needsAlphanumeric = normalized.platform === "kuaishou";
      
      // Strict validation for room IDs
      if (needsNumeric && !/^\d+$/.test(normalized.roomId)) {
        invalidCount += 1;
        continue;
      }
      if (needsAlphanumeric && !isValidKuaishouRoomId(normalized.roomId)) {
        invalidCount += 1;
        continue;
      }
      
      // Ignore anything that looks like a title or sentence (contains Chinese characters)
      if (/[\u4e00-\u9fa5]/.test(normalized.roomId)) {
        invalidCount += 1;
        continue;
      }

      // Ignore very short strings that are just punctuation or symbols
      if (normalized.roomId.length < 2 && !/^[a-z0-9]$/i.test(normalized.roomId)) {
        invalidCount += 1;
        continue;
      }
      const key = `${normalized.platform}:${normalized.roomId}`;
      if (seen.has(key)) {
        continue;
      }
      seen.add(key);
      entries.push(normalized);
    }

    return { entries, invalidCount };
  }

  function applyBatchNormalization() {
    const { entries, invalidCount } = buildBatchEntries(
      batchInput,
      selectedPlatform
    );
    const normalizedLines = entries.map((entry) =>
      entry.platform === selectedPlatform
        ? entry.roomId
        : `${entry.platform} ${entry.roomId}`
    );
    batchInput = normalizedLines.join("\n");
    batchValidCount = entries.length;
    batchInvalidCount = invalidCount;
  }

  function addNewRecorder(room_id: string, platform: string, extra: string) {
    if (extra.includes("?")) {
      extra = extra.split("?")[0];
    }

    invoke("add_recorder", {
      roomId: room_id,
      platform: platform,
      extra: extra,
    })
      .then(() => {
        addModal = false;
        addRoom = "";
      })
      .catch(async (e) => {
        await message(e);
      });
  }

  async function addBatchRecorders() {
    if (batchEntries.length === 0) {
      await message("没有可添加的直播间");
      return;
    }
    let success = 0;
    let failed = 0;
    const failures: { platform: string; roomId: string; error: string }[] = [];
    for (const entry of batchEntries) {
      try {
        await invoke("add_recorder", {
          roomId: entry.roomId,
          platform: entry.platform,
          extra: "",
        });
        success += 1;
      } catch (e) {
        failed += 1;
        failures.push({
          platform: entry.platform,
          roomId: entry.roomId,
          error: String(e),
        });
      }
    }
    addModal = false;
    addRoom = "";
    batchInput = "";
    const detail = failed > 0 ? `，失败 ${failed} 个` : "";
    const summary = `成功 ${success} 个${detail}`;
    if (failures.length > 0) {
      const maxLines = 20;
      const lines = failures
        .slice(0, maxLines)
        .map((failure) => `${failure.platform}:${failure.roomId} - ${failure.error}`);
      const more =
        failures.length > maxLines
          ? `\\n... (${failures.length - maxLines} 条)`
          : "";
      await message(
        `${summary}\\n\\n失败明细:\\n${lines.join("\\n")}${more}`
      );
    } else {
      await message(summary);
    }
  }

  function handleWholeClipGenerated() {
    generateWholeClipModal = false;
    generateWholeClipArchive = null;
  }

  onMount(async () => {
    await onOpenUrl((urls: string[]) => {
      console.log("Received Deep Link:", urls);
      if (urls.length > 0) {
        const url = urls[0];

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
          if (room_id.startsWith("u/")) {
            room_id = room_id.slice(2);
          }
          platform = "kuaishou";
        }

        if (url.startsWith("bsr://live.tiktok.com/")) {
          room_id = url.replace("bsr://live.tiktok.com/", "").split("?")[0];
          platform = "tiktok";
        }

        if (platform && room_id) {
          addModal = true;
          addRoom = room_id;
          selectedPlatform = platform;

          const needsNumeric = platform === "bilibili" || platform === "douyin";
          if (!needsNumeric || Number.isInteger(Number(room_id))) {
            addValid = true;
            addErrorMsg = "";
          } else {
            addErrorMsg = "ID格式错误，请检查输入";
            addValid = false;
          }
        }
      }
    });
  });

  function openGenerateWholeClipModal(archive: RecordItem) {
    generateWholeClipArchive = archive;
    generateWholeClipModal = true;
  }

  onDestroy(() => {
    if (summaryTimer) {
      clearInterval(summaryTimer);
      summaryTimer = null;
    }
    if (dragMoveRaf) {
      cancelAnimationFrame(dragMoveRaf);
      dragMoveRaf = 0;
    }
  });

</script>

<div
  class="flex-1 p-6 overflow-auto custom-scrollbar-light bg-gray-50 dark:bg-black"
>
  <div class="space-y-6">
    <!-- Header -->
    <div class="flex justify-between items-center">
      <div class="flex items-center space-x-4">
        <h1 class="text-2xl font-semibold text-gray-900 dark:text-white">
          直播间
        </h1>
        <div
          class="flex items-center space-x-2 text-sm text-gray-500 dark:text-gray-400"
        >
          <span class="flex items-center space-x-1">
            <span class="w-2 h-2 rounded-full bg-green-500"></span>
            <span>{room_active} 直播中</span>
          </span>
          <span>•</span>
          <span>{room_inactive} 未直播</span>
        </div>
      </div>
      <div class="flex items-center space-x-3">
        <div class="relative">
          <Search
            class="w-5 h-5 absolute left-3 top-1/2 transform -translate-y-1/2 text-gray-400 dark:text-gray-500"
          />
          <input
            type="text"
            bind:value={searchQuery}
            placeholder="搜索直播间..."
            class="pl-10 pr-4 py-2 rounded-lg bg-gray-100 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 focus:outline-none focus:ring-2 focus:ring-blue-500 dark:focus:ring-blue-400 text-gray-900 dark:text-white"
          />
        </div>
        <button
          class={"px-3 py-2 rounded-lg border text-sm transition-colors " +
            (multiSelect
              ? "bg-blue-500 text-white border-blue-500"
              : "bg-white dark:bg-[#3c3c3e] text-gray-700 dark:text-gray-200 border-gray-200 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700")}
          on:click={() => {
            multiSelect = !multiSelect;
            if (!multiSelect) {
              clearRoomSelection();
            }
          }}
        >
          多选
        </button>
        {#if multiSelect}
          <button
            class="px-3 py-2 rounded-lg border text-sm bg-white dark:bg-[#3c3c3e] text-gray-700 dark:text-gray-200 border-gray-200 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700"
            on:click={() => selectAllFiltered()}
          >
            全选
          </button>
          <button
            class="px-3 py-2 rounded-lg border text-sm bg-white dark:bg-[#3c3c3e] text-gray-700 dark:text-gray-200 border-gray-200 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700"
            on:click={() => clearRoomSelection()}
          >
            清空
          </button>
          <button
            class={"px-3 py-2 rounded-lg border text-sm transition-colors " +
              (selectedRoomCount === 0
                ? "bg-gray-200 dark:bg-gray-700 text-gray-400 border-gray-200 dark:border-gray-700 cursor-not-allowed"
                : "bg-red-600 text-white border-red-600 hover:bg-red-700")}
            on:click={() => {
              if (selectedRoomCount === 0) return;
              deleteMode = "bulk";
              deleteRooms = getSelectedRooms();
              deleteRoom = null;
              deleteModal = true;
            }}
            disabled={selectedRoomCount === 0}
          >
            删除选中({selectedRoomCount})
          </button>
        {/if}
        <button
          class="px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600 transition-colors flex items-center space-x-2"
          on:click={() => {
            addModal = true;
          }}
        >
          <Plus class="w-5 h-5 icon-white" />
          <span>添加新直播间</span>
        </button>
      </div>
    </div>

    <!-- Room Grid -->
    <div
      class="grid grid-cols-3 gap-4 overflow-visible"
      bind:this={gridRef}
      on:mousedown={handleGridMouseDown}
    >
      <!-- Active Room Card -->
      {#each filteredRecorders as room (roomKey(room))}
        <div
          use:cardRef={room}
          role="button"
          tabindex="0"
          class={"p-4 rounded-xl bg-white dark:bg-[#3c3c3e] border border-gray-200 dark:border-gray-700 hover:border-blue-500 dark:hover:border-blue-400 transition-colors " +
            "relative overflow-visible " +
            (multiSelect && isRoomSelected(room)
              ? "ring-2 ring-blue-500 border-blue-500"
              : "")}
          on:click={(event) => handleRoomClick(event, room)}
          on:keydown={(event) => {
            if (event.key === "Enter" || event.key === " ") {
              handleRoomClick(event, room);
            }
          }}
        >
          <div class="relative overflow-visible">
            <img
              src={room.room_info.room_cover}
              alt="cover"
              class={"w-full h-40 object-cover rounded-lg " +
                (room.room_info.status ? "" : "brightness-75")}
            />
            <!-- Room ID watermark -->
            <div
              class="absolute bottom-2 left-2 px-2 py-1 rounded-md bg-black/30 backdrop-blur-sm flex items-center"
            >
              <span class="text-xs text-white/80 font-mono"
                >{room.room_info.platform.toUpperCase()}#{room.room_info
                  .room_id}</span
              >
            </div>
            {#if room.enabled}
              <div
                class={"absolute top-2 left-2 p-1.5 px-2 rounded-md text-white text-xs flex items-center justify-center " +
                  (room.recording ? "bg-red-500" : "bg-gray-700/90")}
              >
                <AutoRecordIcon class="w-4 h-4 text-white" />
                {#if room.recording}
                  <span class="text-white ml-1">录制中</span>
                {/if}
              </div>
            {/if}
            {#if !room.room_info.status}
              <div
                class={"absolute bottom-2 right-2 p-1.5 px-2 rounded-md text-white text-xs flex items-center justify-center bg-gray-700"}
              >
                <span>直播未开始</span>
              </div>
            {:else}
              <div
                class={"absolute bottom-2 right-2 p-1.5 px-2 rounded-md text-white text-xs flex items-center justify-center bg-green-500"}
              >
                <span>直播进行中</span>
              </div>
            {/if}
            <button
              type="button"
              class="absolute top-2 right-2 z-30 px-2 py-1.5 rounded-lg bg-gray-900/50 hover:bg-gray-900/70 transition-colors"
              title={room.enabled ? "关闭直播间" : "启用直播间"}
              data-ignore-selection
              on:click|stopPropagation
            >
              <span class="toggle-switch">
                <input
                  type="checkbox"
                  checked={room.enabled}
                  aria-label={room.enabled ? "关闭直播间" : "启用直播间"}
                  on:click|stopPropagation
                  on:change={async () => {
                    await toggleEnabled(room);
                  }}
                />
                <span class="toggle-slider"></span>
              </span>
            </button>
          </div>
          <div class="mt-3 space-y-2">
            <div class="flex items-start justify-between">
              <div>
                <div class="flex items-center space-x-2">
                  {#if room.room_info.platform === "bilibili"}
                    <BilibiliIcon class="w-4 h-4" />
                  {:else if room.room_info.platform === "douyin"}
                    <DouyinIcon class="w-4 h-4" />
                  {:else if room.room_info.platform === "kuaishou"}
                    <KuaishouIcon class="w-4 h-4" />
                  {:else if room.room_info.platform === "huya"}
                    <HuyaIcon class="w-4 h-4" />
                  {:else if room.room_info.platform === "tiktok"}
                    <TikTokIcon class="w-5 h-5" />
                  {:else if room.room_info.platform === "wechat"}
                    <img src="/imgs/wechat.svg" alt="视频号" class="w-5 h-5" />
                  {:else}
                    <Globe class="w-4 h-4 text-gray-400" />
                  {/if}
                  <h3 class="font-medium text-gray-900 dark:text-white">
                    <button
                      type="button"
                      class="text-left hover:underline decoration-gray-400/70 underline-offset-2"
                      title="打开直播间"
                      on:click={() => {
                        openLiveUrl(room);
                      }}
                    >
                      {room.room_info.room_title}
                    </button>
                  </h3>
                </div>
              </div>
            </div>
            <div class="flex items-center justify-between">
              <div
                class="flex items-center space-x-2 text-sm text-gray-600 dark:text-gray-400"
              >
                <button
                  class="flex items-center space-x-2 p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                  on:click={() => {
                    openUserUrl(room);
                  }}
                >
                  <img
                    src={room.user_info.user_avatar}
                    alt="avatar"
                    class="w-8 h-8 rounded-full"
                  />
                  <span>{room.user_info.user_name}</span>
                </button>
              </div>
              <div class="flex items-center space-x-1">
                <button
                  class="p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                  title={room.recording ? "播放预览" : "打开网页直播间"}
                  on:click={() => {
                    openLivePreview(room);
                  }}
                >
                  <Play class="w-5 h-5 dark:icon-white" />
                </button>
                <button
                  class={"px-2.5 py-1 text-xs rounded-lg border text-gray-700 dark:text-gray-200 " +
                    (reloadPendingKeys.has(roomKey(room))
                      ? "border-gray-200 dark:border-gray-700 bg-gray-100 dark:bg-gray-700 cursor-not-allowed opacity-70"
                      : "border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700")}
                  title="重载直播间"
                  disabled={reloadPendingKeys.has(roomKey(room))}
                  on:click={async () => {
                    await reloadRoom(room);
                  }}
                >
                  {reloadPendingKeys.has(roomKey(room)) ? "重载中" : "重载"}
                </button>
                <button
                  class="p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                  title="移除直播间"
                  on:click={() => {
                    deleteMode = "single";
                    deleteRoom = room;
                    deleteRooms = [room];
                    deleteModal = true;
                  }}
                >
                  <Trash2 class="w-5 h-5 text-red-500" />
                </button>
                <button
                  class="p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                  on:click={() => {
                    archiveRoom = room;
                    showArchives(room.room_info.room_id);
                  }}
                >
                  <History class="w-5 h-5 dark:icon-white" />
                </button>
              </div>
            </div>
          </div>
        </div>
      {/each}

      <!-- Add Room Card -->
      <button
        class="p-4 rounded-xl border-2 border-dashed border-gray-300 dark:border-gray-600 hover:border-blue-500 dark:hover:border-blue-400 transition-colors flex flex-col items-center justify-center space-y-2"
        on:click={() => {
          addModal = true;
        }}
      >
        <div
          class="w-12 h-12 rounded-full bg-blue-500/10 flex items-center justify-center"
        >
          <Plus class="w-6 h-6 icon-primary" />
        </div>
        <span class="text-sm font-medium text-blue-600 dark:text-blue-400"
          >添加新直播间</span
        >
        <span class="text-xs text-gray-500 dark:text-gray-400"
          >配置一个新直播间以及其相关设置</span
        >
      </button>
    </div>
  </div>
</div>
{#if deleteModal}
  <div
    class="fixed inset-0 bg-black/20 dark:bg-black/40 backdrop-blur-sm z-50 flex items-center justify-center"
    transition:fade={{ duration: 200 }}
  >
    <div
      class="mac-modal delete-modal w-[320px] bg-white dark:bg-[#323234] rounded-xl shadow-xl overflow-hidden"
      transition:scale={{ duration: 150, start: 0.95 }}
    >
      <div class="p-6 space-y-4">
        <div class="text-center space-y-2">
          <h3 class="text-base font-medium text-gray-900 dark:text-white">
            {deleteMode === "bulk" ? "批量移除直播间" : "移除直播间"}
          </h3>
          <p class="text-sm text-gray-500 dark:text-gray-400">
            {deleteMode === "bulk"
              ? `将移除选中的 ${deleteRooms.length} 个直播间`
              : "此操作将移除该直播间"}
          </p>
        </div>
        <div class="flex justify-center space-x-3">
          <button
            class="w-24 px-4 py-2 text-sm font-medium text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-600 rounded-lg transition-colors"
            on:click={() => {
              deleteModal = false;
            }}
          >
            取消
          </button>
          <button
            class="w-24 px-4 py-2 bg-red-600 hover:bg-red-700 text-white text-sm font-medium rounded-lg transition-colors"
            on:click={async () => {
              if (deleteMode === "bulk") {
                const targets = deleteRooms;
                for (const room of targets) {
                  try {
                    await invoke("remove_recorder", {
                      roomId: room.room_info.room_id,
                      platform: room.room_info.platform,
                    });
                  } catch (e) {
                    console.error("Failed to remove recorder:", e);
                  }
                }
                clearRoomSelection();
              } else if (deleteRoom) {
                await invoke("remove_recorder", {
                  roomId: deleteRoom.room_info.room_id,
                  platform: deleteRoom.room_info.platform,
                });
              }
              deleteModal = false;
              deleteRoom = null;
              deleteRooms = [];
              await update_summary();
            }}
          >
            移除
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}

{#if addModal}
  <div
    class="fixed inset-0 bg-black/20 dark:bg-black/40 backdrop-blur-sm z-50 flex items-center justify-center"
    transition:fade={{ duration: 200 }}
  >
    <div
      class="mac-modal add-modal w-[400px] bg-white dark:bg-[#323234] rounded-xl shadow-xl overflow-hidden"
      transition:scale={{ duration: 150, start: 0.95 }}
    >
      <!-- Header -->
      <div class="px-6 py-4 border-b border-gray-200 dark:border-gray-700/50">
        <h2 class="text-base font-medium text-gray-900 dark:text-white">
          添加直播间
        </h2>
      </div>

      <div class="p-6 space-y-6">
        <div class="space-y-4">
          <div class="space-y-2">
            <div
              id="platform-label"
              class="block text-sm font-medium text-gray-700 dark:text-gray-300"
            >
              平台
            </div>
            <div
              class="grid grid-cols-3 gap-2 p-0.5 bg-[#f5f5f7] dark:bg-[#1c1c1e] rounded-lg"
              role="group"
              aria-labelledby="platform-label"
            >
              <button
                class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
                'bilibili'
                  ? 'bg-white dark:bg-[#323234] shadow-sm text-gray-900 dark:text-white'
                  : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
                on:click={() => (selectedPlatform = "bilibili")}
              >
                Bilibili
              </button>
              <button
                class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
                'douyin'
                  ? 'bg-white dark:bg-[#323234] shadow-sm text-gray-900 dark:text-white'
                  : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
                on:click={() => (selectedPlatform = "douyin")}
              >
                抖音
              </button>
              <button
                class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
                'huya'
                  ? 'bg-white dark:bg-[#323234] shadow-sm text-gray-900 dark:text-white'
                  : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
                on:click={() => (selectedPlatform = "huya")}
              >
                虎牙
              </button>
              <button
                class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
                'kuaishou'
                  ? 'bg-white dark:bg-[#323234] shadow-sm text-gray-900 dark:text-white'
                  : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
                on:click={() => (selectedPlatform = "kuaishou")}
              >
                快手
              </button>
              <button
                class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform ===
                'tiktok'
                  ? 'bg-white dark:bg-[#323234] shadow-sm text-gray-900 dark:text-white'
                  : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
                on:click={() => (selectedPlatform = "tiktok")}
              >
                TikTok
              </button>
              <button
                class="px-3 py-2 text-sm font-medium rounded-md transition-colors {selectedPlatform === 'wechat' ? 'bg-white dark:bg-[#323234] shadow-sm text-gray-900 dark:text-white' : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
                on:click={() => { selectedPlatform = "wechat"; batchMode = false; }}
              >视频号</button>
            </div>
          </div>

          <div class="space-y-2">
            <div
              id="add-mode-label"
              class="block text-sm font-medium text-gray-700 dark:text-gray-300"
            >
              添加模式
            </div>
            <div
              class="grid grid-cols-2 gap-2 p-0.5 bg-[#f5f5f7] dark:bg-[#1c1c1e] rounded-lg"
              role="group"
              aria-labelledby="add-mode-label"
            >
              <button
                class="px-3 py-2 text-sm font-medium rounded-md transition-colors {batchMode
                ? 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'
                : 'bg-white dark:bg-[#323234] shadow-sm text-gray-900 dark:text-white'}"
                on:click={() => {
                  batchMode = false;
                }}
              >
                单个
              </button>
              <button
                class="px-3 py-2 text-sm font-medium rounded-md transition-colors {batchMode
                ? 'bg-white dark:bg-[#323234] shadow-sm text-gray-900 dark:text-white'
                : 'text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-white'}"
                on:click={() => {
                  if (selectedPlatform !== "wechat") batchMode = true;
                }}
              >
                批量
              </button>
            </div>
          </div>

          <div class="space-y-2">
            <label
              for="room_id"
              class="block text-sm font-medium text-gray-700 dark:text-gray-300"
            >
              {selectedPlatform === "wechat" ? "媒体直链 / 资源 JSON" : selectedPlatform === "kuaishou" || selectedPlatform === "tiktok"
                ? "直播间链接"
                : selectedPlatform === "bilibili" || selectedPlatform === "douyin"
                  ? "直播间链接/ID"
                  : "直播间ID"}
            </label>
            {#if selectedPlatform === "wechat"}
              <textarea
                id="room_id" rows="5" bind:value={addRoom}
                class="w-full px-3 py-2 bg-[#f5f5f7] dark:bg-[#1c1c1e] border-0 rounded-lg focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-white"
                placeholder="粘贴参考工具复制的 m3u8 / flv / mp4 直链，或单条资源 JSON"
              ></textarea>
              <p class="text-xs text-gray-500 dark:text-gray-400">
                使用您有权保存的媒体。兼容 wechatVideoDownload 的复制链接、res-downloader 的资源 JSON（Url / objectDesc）。不支持分享页直解、加密短视频或弹幕；不会安装证书、更改系统代理或监听微信。流结束或失败后自动停止，过期链接请重新捕获后添加。
              </p>
            {:else if batchMode}
              <textarea
                id="room_id"
                rows="6"
                bind:value={batchInput}
                class="w-full px-3 py-2 bg-[#f5f5f7] dark:bg-[#1c1c1e] border-0 rounded-lg focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-white placeholder-gray-500 dark:placeholder-gray-400"
                placeholder="批量粘贴直播间链接/ID，可包含标题或其他文字"
              ></textarea>
              <div class="text-xs text-gray-500 dark:text-gray-400">
                检测到 {batchValidCount} 个可添加，忽略 {batchInvalidCount} 个
              </div>
              <button
                class="mt-2 text-xs text-blue-600 dark:text-blue-400 hover:underline"
                on:click={() => applyBatchNormalization()}
              >
                {"一键去重/校验"}
              </button>
            {:else}
              <input
                id="room_id"
                type="text"
                bind:value={addRoom}
                class="w-full px-3 py-2 bg-[#f5f5f7] dark:bg-[#1c1c1e] border-0 rounded-lg focus:ring-2 focus:ring-blue-500 text-gray-900 dark:text-white placeholder-gray-500 dark:placeholder-gray-400"
                placeholder={selectedPlatform === "kuaishou" || selectedPlatform === "tiktok"
                  ? "请输入直播间链接"
                  : selectedPlatform === "bilibili" || selectedPlatform === "douyin"
                    ? "请输入直播间链接或房间号"
                    : "请输入直播间房间号"}
              />
            {/if}
            {#if addErrorMsg}
              <p class="text-sm text-red-600 dark:text-red-500">
                {addErrorMsg}
              </p>
            {/if}
          </div>

          <div class="flex justify-end space-x-3">
            <button
              class="px-4 py-2 text-sm font-medium text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-600 rounded-lg transition-colors"
              on:click={() => {
                addModal = false;
              }}
            >
              取消
            </button>
            <button
              class="px-4 py-2 bg-[#0A84FF] hover:bg-[#0A84FF]/90 text-white text-sm font-medium rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              disabled={!addValid}
              on:click={() => {
                if (batchMode) {
                  addBatchRecorders();
                  return;
                }
                const normalized = normalizeRoomInput(
                  addRoom,
                  selectedPlatform
                );
                addNewRecorder(normalized.roomId, normalized.platform, "");
                addModal = false;
                addRoom = "";
              }}
            >
              添加
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
{/if}

{#if archiveModal}
  <div
    class="fixed inset-0 bg-black/20 dark:bg-black/40 backdrop-blur-sm z-50 flex items-center justify-center"
    transition:fade={{ duration: 200 }}
  >
    <div
      class="mac-modal archive-modal w-[900px] bg-white dark:bg-[#323234] rounded-xl shadow-xl overflow-hidden flex flex-col max-h-[80vh]"
      transition:scale={{ duration: 150, start: 0.95 }}
    >
      <!-- Header -->
      <div
        class="flex justify-between items-center px-6 py-4 border-b border-gray-200 dark:border-gray-700/50"
      >
        <div class="flex items-center space-x-3">
          <h2 class="text-base font-medium text-gray-900 dark:text-white">
            直播间记录
          </h2>
          <span class="text-sm text-gray-500 dark:text-gray-400">
            {archiveRoom?.user_info.user_name} · {archiveRoom?.room_info
              .room_id}
          </span>
        </div>
        <button
          class="p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700/50 transition-colors"
          on:click={() => (archiveModal = false)}
        >
          <X class="w-5 h-5 dark:icon-white" />
        </button>
      </div>

      <div
        class="flex-1 overflow-auto custom-scrollbar-light"
        on:scroll={handleScroll}
      >
        <div class="p-6">
          <div class="overflow-x-auto custom-scrollbar-light">
            <table class="w-full">
              <thead>
                <tr class="border-b border-gray-200 dark:border-gray-700/50">
                  <th
                    class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                    >直播时间</th
                  >
                  <th
                    class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                    >标题</th
                  >
                  <th
                    class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                    >时长</th
                  >
                  <th
                    class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                    >大小</th
                  >
                  <th
                    class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                    >码率</th
                  >
                  <th
                    class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                    >操作</th
                  >
                </tr>
              </thead>
              <tbody class="divide-y divide-gray-200 dark:divide-gray-700/50">
                {#each archives as archive (archive.parent_id + ":" + archive.live_id)}
                  <tr
                    class="group hover:bg-[#f5f5f7] dark:hover:bg-[#3a3a3c] transition-colors"
                  >
                    <td class="px-4 py-3">
                      <div class="flex flex-col">
                        <span class="text-sm text-gray-900 dark:text-white"
                          >{format_ts(archive.created_at).split(" ")[0]}</span
                        >
                        <span class="text-xs text-gray-500 dark:text-gray-400"
                          >{format_ts(archive.created_at).split(" ")[1]}</span
                        >
                      </div>
                    </td>
                    <td class="px-4 py-3">
                      <div class="flex items-center space-x-3">
                        <img
                          src={archive.cover}
                          alt="cover"
                          class="w-12 h-8 rounded object-cover"
                        />
                        <span class="text-sm text-gray-900 dark:text-white"
                          >{archive.title}</span
                        >
                      </div>
                    </td>
                    <td class="px-4 py-3 text-sm text-gray-900 dark:text-white"
                      >{format_duration(archive.length)}</td
                    >
                    <td class="px-4 py-3 text-sm text-gray-900 dark:text-white"
                      >{format_size(archive.size)}</td
                    >
                    <td
                      class="px-4 py-3 text-sm text-gray-500 dark:text-gray-400"
                      >{calc_bitrate(archive.size, archive.length)} Kbps</td
                    >
                    <td class="px-4 py-3">
                      <div class="flex items-center space-x-2">
                        <button
                          class="p-1.5 rounded-lg hover:bg-blue-500/10 transition-colors"
                          title="预览录播"
                          on:click={() => {
                            invoke("open_live", {
                              platform: archiveRoom.room_info.platform,
                              roomId: archiveRoom.room_info.room_id,
                              liveId: archive.live_id,
                            });
                          }}
                        >
                          <PlayIcon class="w-4 h-4 icon-primary" />
                        </button>
                        <button
                          class="p-1.5 rounded-lg hover:bg-blue-500/10 transition-colors"
                          title="生成完整切片"
                          on:click={() => {
                            openGenerateWholeClipModal(archive);
                          }}
                        >
                          <FileVideo class="w-4 h-4 icon-primary" />
                        </button>
                        <button
                          class="p-1.5 rounded-lg hover:bg-red-500/10 transition-colors"
                          title="删除记录"
                          on:click={() => {
                            invoke("delete_archive", {
                              platform: archiveRoom.room_info.platform,
                              roomId: archiveRoom.room_info.room_id,
                              liveId: archive.live_id,
                            })
                              .then(async () => {
                                // 删除后重新加载第一页数据
                                currentPage = 0;
                                archives = [];
                                hasMore = true;
                                loadError = "";
                                await updateArchives();
                              })
                              .catch((e) => {
                                alert(e);
                              });
                          }}
                        >
                          <Trash2 class="w-4 h-4 text-red-500" />
                        </button>
                      </div>
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>

          <!-- 加载更多区域 -->
          <div class="mt-6 flex justify-center">
            {#if loadError}
              <div class="text-red-500 dark:text-red-400 text-sm mb-3">
                {loadError}
                <button
                  class="ml-2 text-blue-500 hover:text-blue-600 underline"
                  on:click={() => {
                    loadError = "";
                    loadMoreArchives();
                  }}
                >
                  重试
                </button>
              </div>
            {/if}

            {#if isLoading && currentPage === 0}
              <div
                class="flex items-center space-x-2 text-gray-500 dark:text-gray-400"
              >
                <div
                  class="animate-spin rounded-full h-5 w-5 border-b-2 border-blue-500"
                ></div>
                <span>加载中...</span>
              </div>
            {:else if isLoading}
              <div
                class="flex items-center space-x-2 text-gray-500 dark:text-gray-400"
              >
                <div
                  class="animate-spin rounded-full h-4 w-4 border-b-2 border-blue-500"
                ></div>
                <span>加载更多...</span>
              </div>
            {:else if archives.length === 0 && !isLoading}
              <div class="text-gray-500 dark:text-gray-400 text-sm">
                暂无录制记录
              </div>
            {/if}
          </div>
        </div>
      </div>
    </div>
  </div>
{/if}

<!-- Generate Whole Clip Modal -->
<GenerateWholeClipModal
  bind:showModal={generateWholeClipModal}
  archive={generateWholeClipArchive}
  roomId={generateWholeClipArchive?.room_id || ""}
  platform={generateWholeClipArchive?.platform || ""}
  on:generated={handleWholeClipGenerated}
/>

<svelte:window
  on:mousedown={handleModalClickOutside}
  on:mousemove={handleDragMove}
  on:mouseup={handleDragEnd}
/>

{#if dragRect}
  <div
    class="fixed pointer-events-none border border-blue-500/80 bg-blue-500/10 rounded"
    style={`left:${dragRect.left}px;top:${dragRect.top}px;width:${dragRect.width}px;height:${dragRect.height}px;`}
  ></div>
{/if}

<style>
  @keyframes spin-slow {
    from {
      transform: rotate(0deg);
    }
    to {
      transform: rotate(360deg);
    }
  }

  .animate-spin-slow {
    animation: spin-slow 3s linear infinite;
  }

  .toggle-switch {
    position: relative;
    display: inline-block;
    width: 36px;
    height: 20px;
    flex-shrink: 0;
  }

  .toggle-switch input {
    position: absolute;
    inset: 0;
    opacity: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    cursor: pointer;
    z-index: 2;
  }

  .toggle-slider {
    position: absolute;
    inset: 0;
    background-color: rgba(120, 120, 128, 0.32);
    transition: 0.2s ease;
    border-radius: 999px;
    pointer-events: none;
  }

  .toggle-slider:before {
    position: absolute;
    content: "";
    height: 16px;
    width: 16px;
    left: 2px;
    bottom: 2px;
    background-color: #fff;
    transition: 0.2s ease;
    border-radius: 50%;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.18);
  }

  .toggle-switch input:checked + .toggle-slider {
    background-color: #34c759;
  }

  .toggle-switch input:checked + .toggle-slider:before {
    transform: translateX(16px);
  }
</style>
