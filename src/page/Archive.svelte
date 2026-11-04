<script lang="ts">
  import { invoke, get, get_static_url } from "../lib/invoker";
  import type { RecordItem } from "../lib/db";
  import { onMount } from "svelte";
  import {
    Play,
    Trash2,
    Calendar,
    Clock,
    HardDrive,
    RefreshCw,
    ChevronDown,
    ChevronUp,
    Video,
    Globe,
    Home,
    FileVideo,
    History,
  } from "lucide-svelte";
  import BilibiliIcon from "../lib/components/BilibiliIcon.svelte";
  import DouyinIcon from "../lib/components/DouyinIcon.svelte";
  import KuaishouIcon from "../lib/components/KuaishouIcon.svelte";
  import HuyaIcon from "../lib/components/HuyaIcon.svelte";
  import TikTokIcon from "../lib/components/TikTokIcon.svelte";
  import GenerateWholeClipModal from "../lib/components/GenerateWholeClipModal.svelte";
  import type { RecorderInfo, RecorderList } from "src/lib/interface";

  let archives: RecordItem[] = [];
  let filteredArchives: RecordItem[] = [];
  let loading = false;
  let sortBy = "created_at";
  let sortOrder = "desc";
  type RoomOption = {
    id: string;
    label: string;
  };

  let selectedRoomId: string | null = null;
  let roomOptions: RoomOption[] = [];

  let selectedArchives: Set<string> = new Set();
  let showDeleteConfirm = false;
  let archiveToDelete: RecordItem | null = null;
  let showDeleteZeroConfirm = false;
  let deleteSizeThresholdMb = 1;
  let autoRoomInfoRefreshDone = false;

  // 生成完整录播相关状态
  let showWholeClipModal = false;
  let wholeClipArchive: RecordItem | null = null;

  // 分页相关状态
  let currentPage = 1;
  let pageSize = 20;
  let totalPages = 1;
  let totalCount = 0;
  let isLoading = false;
  let loadError = "";

  // 页面大小选项
  const pageSizeOptions = [10, 20, 50, 100];

  // 所有数据缓存
  let allArchives = [];
  let allRooms: RecorderInfo[] = [];

  const room_cover_cache: Map<string, string> = new Map();
  const room_cover_fallback: Map<string, string> = new Map();

  function default_cover(platform: string) {
    const coverMap = {
      bilibili: "/imgs/bilibili.png",
      douyin: "/imgs/douyin.svg",
      huya: "/imgs/huya.png",
      kuaishou: "/imgs/kuaishou.svg",
      tiktok: "/imgs/Tiktok.svg",
    };
    return coverMap[platform] || "/imgs/huya.png";
  }

  function is_remote_url(url: string) {
    return /^https?:\/\//i.test(url);
  }

  function is_default_cover_url(src: string, platform: string) {
    const fallback = default_cover(platform);
    return src.includes(fallback);
  }

  async function get_room_cover(room_id: string, url: string) {
    const cache_key = `${room_id}:${url}`;
    if (room_cover_cache.has(cache_key)) {
      return room_cover_cache.get(cache_key);
    }
    const response = await get(url);
    const blob = await response.blob();
    const cover_url = URL.createObjectURL(blob);
    room_cover_cache.set(cache_key, cover_url);
    return cover_url;
  }

  async function refresh_room_cover_fallback() {
    room_cover_fallback.clear();
    for (const room of allRooms) {
      let cover =
        room.room_info.room_cover ||
        room.user_info.user_avatar ||
        default_cover(room.room_info.platform);
      if (cover && is_remote_url(cover)) {
        cover = await get_room_cover(room.room_info.room_id, cover);
      }
      room_cover_fallback.set(room.room_info.room_id, cover);
    }
    for (const archive of allArchives) {
      if (!room_cover_fallback.has(archive.room_id)) {
        room_cover_fallback.set(
          archive.room_id,
          default_cover(archive.platform),
        );
      }
    }
  }

  function handle_archive_cover_load(event: Event, archive: RecordItem) {
    const target = event.currentTarget;
    if (!(target instanceof HTMLImageElement)) {
      return;
    }
    const src = target.currentSrc || target.src;
    if (!is_default_cover_url(src, archive.platform)) {
      room_cover_fallback.set(archive.room_id, src);
    }
  }

  function handle_archive_cover_error(event: Event, archive: RecordItem) {
    const target = event.currentTarget;
    if (!(target instanceof HTMLImageElement)) {
      return;
    }
    const fallback =
      room_cover_fallback.get(archive.room_id) ||
      default_cover(archive.platform);
    target.src = fallback;
  }

  onMount(async () => {
    // 从本地存储恢复分页大小设置
    const savedPageSize = localStorage.getItem("archive-page-size");
    if (savedPageSize && pageSizeOptions.includes(parseInt(savedPageSize))) {
      pageSize = parseInt(savedPageSize);
    }

    await loadArchives();
  });

  /**
   * 初始化加载所有录播数据
   */
  async function loadArchives() {
    if (isLoading) return;

    isLoading = true;
    loading = true;
    loadError = "";

    try {
      if (!autoRoomInfoRefreshDone) {
        autoRoomInfoRefreshDone = true;
        invoke("refresh_archive_room_info").catch((error) => {
          console.warn("Failed to refresh archive room info:", error);
        });
      }

      // 获取所有直播间列表
      const recorderList: RecorderList = await invoke("get_recorder_list");
      allRooms = recorderList.recorders || [];

      // ??
      allArchives = [];
      let offset = 0;
      const fetchLimit = 200;
      while (true) {
        const batch = await invoke<RecordItem[]>("get_recent_record", {
          roomId: "",
          offset,
          limit: fetchLimit,
        });
        if (!batch.length) {
          break;
        }
        for (const archive of batch) {
          archive.cover = await get_static_url(
            "cache",
            `${archive.platform}/${archive.room_id}/${archive.live_id}/cover.jpg`,
          );
        }
        allArchives = [...allArchives, ...batch];
        if (batch.length < fetchLimit) {
          break;
        }
        offset += fetchLimit;
      }

      await refresh_room_cover_fallback();

      // ???
      const roomOptionMap: Map<string, RoomOption> = new Map();
      for (const room of allRooms) {
        const id = room.room_info.room_id;
        const name =
          room.user_info?.user_name?.trim() ||
          room.room_info?.room_title?.trim() ||
          "";
        const label = name ? `${name} (${id})` : id;
        roomOptionMap.set(id, { id, label });
      }
      for (const archive of allArchives) {
        if (!roomOptionMap.has(archive.room_id)) {
          roomOptionMap.set(archive.room_id, {
            id: archive.room_id,
            label: archive.room_id,
          });
        }
      }
      roomOptions = Array.from(roomOptionMap.values()).sort((a, b) =>
        a.label.localeCompare(b.label),
      );

      allArchives.sort((a, b) => {
        return (
          new Date(b.created_at).getTime() - new Date(a.created_at).getTime()
        );
      });

      totalCount = allArchives.length;
      updatePagination();
    } catch (error) {
      console.error("Failed to load archives:", error);
      loadError = "加载失败，请重试";
    } finally {
      isLoading = false;
      loading = false;
    }
  }

  /**
   * 更新分页信息和当前页数据
   */
  function updatePagination() {
    totalPages = Math.ceil(totalCount / pageSize);
    if (currentPage > totalPages) {
      currentPage = totalPages || 1;
    }
    applyFilters();
  }

  /**
   * 跳转到指定页面
   */
  function goToPage(page: number) {
    if (page < 1 || page > totalPages || page === currentPage) return;
    currentPage = page;
    applyFilters();
  }

  /**
   * 上一页
   */
  function prevPage() {
    if (currentPage > 1) {
      currentPage--;
      applyFilters();
    }
  }

  /**
   * 下一页
   */
  function nextPage() {
    if (currentPage < totalPages) {
      currentPage++;
      applyFilters();
    }
  }

  /**
   * 更改页面大小
   */
  function changePageSize(newPageSize: number) {
    pageSize = newPageSize;
    currentPage = 1; // 重置到第一页

    // 保存到本地存储
    localStorage.setItem("archive-page-size", newPageSize.toString());

    applyFilters();
  }

  function applyFilters() {
    let filtered: RecordItem[] = [...allArchives];

    // Apply room filter
    if (selectedRoomId !== null) {
      filtered = filtered.filter(
        (archive) => archive.room_id === selectedRoomId,
      );
    }

    // Apply sorting
    filtered.sort((a, b) => {
      let aValue: any, bValue: any;

      switch (sortBy) {
        case "title":
          aValue = a.title.toLowerCase();
          bValue = b.title.toLowerCase();
          break;
        case "length":
          aValue = a.length;
          bValue = b.length;
          break;
        case "size":
          aValue = a.size;
          bValue = b.size;
          break;
        case "created_at":
          aValue = new Date(a.created_at);
          bValue = new Date(b.created_at);
          break;
        case "room_id":
          aValue = a.room_id;
          bValue = b.room_id;
          break;
        case "platform":
          aValue = (a.platform || "").toLowerCase();
          bValue = (b.platform || "").toLowerCase();
          break;
        default:
          aValue = new Date(a.created_at);
          bValue = new Date(b.created_at);
      }

      if (sortOrder === "asc") {
        return aValue > bValue ? 1 : -1;
      } else {
        return aValue < bValue ? 1 : -1;
      }
    });

    // 更新总数和分页信息
    totalCount = filtered.length;
    totalPages = Math.ceil(totalCount / pageSize);

    // 确保当前页在有效范围内
    if (currentPage > totalPages && totalPages > 0) {
      currentPage = totalPages;
    }

    // Apply pagination
    const startIndex = (currentPage - 1) * pageSize;
    const endIndex = startIndex + pageSize;
    filteredArchives = filtered.slice(startIndex, endIndex);

    // 更新archives用于其他功能
    archives = filtered;
  }

  function formatSize(size: number) {
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

  function formatDuration(seconds: number) {
    seconds = Math.round(seconds);
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const secs = seconds % 60;

    if (hours > 0) {
      return `${hours}:${minutes.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
    } else {
      return `${minutes}:${secs.toString().padStart(2, "0")}`;
    }
  }

  function formatDate(dateString: string) {
    const date = new Date(dateString);
    return date.toLocaleString();
  }

  function formatPlatform(platform: string) {
    switch (platform.toLowerCase()) {
      case "bilibili":
        return "B站";
      case "douyin":
        return "抖音";
      case "huya":
        return "虎牙";
      case "kuaishou":
        return "快手";
      case "tiktok":
        return "TikTok";
      case "wechat":
        return "视频号";
      case "youtube":
        return "YouTube";
      default:
        return platform;
    }
  }

  function getRoomUrl(platform: string, roomId: string) {
    if (roomId.startsWith("http")) {
      return roomId;
    }
    switch (platform.toLowerCase()) {
      case "bilibili":
        return `https://live.bilibili.com/${roomId}`;
      case "douyin":
        return `https://live.douyin.com/${roomId}`;
      case "huya":
        return `https://www.huya.com/${roomId}`;
      case "kuaishou":
        return `https://live.kuaishou.com/u/${roomId}`;
      case "tiktok": {
        const handle = roomId.startsWith("@") ? roomId : `@${roomId}`;
        return `https://www.tiktok.com/${handle}/live`;
      }
      case "youtube":
        return `https://www.youtube.com/channel/${roomId}`;
      default:
        return null;
    }
  }

  function calcBitrate(size: number, duration: number) {
    if (!duration || duration <= 0 || !size || size <= 0) {
      return "0";
    }
    return ((size * 8) / duration / 1024).toFixed(0);
  }

  function getArchiveKey(archive: RecordItem) {
    return `${archive.platform}-${archive.room_id}-${archive.parent_id}-${archive.live_id}`;
  }

  function toggleSort(field: string) {
    if (sortBy === field) {
      sortOrder = sortOrder === "asc" ? "desc" : "asc";
    } else {
      sortBy = field;
      sortOrder = "asc";
    }
    applyFilters();
  }

  function toggleArchiveSelection(liveId: string) {
    if (selectedArchives.has(liveId)) {
      selectedArchives.delete(liveId);
    } else {
      selectedArchives.add(liveId);
    }
    selectedArchives = selectedArchives; // Trigger reactivity
  }

  function selectAllArchives() {
    const currentArchives = filteredArchives;
    if (selectedArchives.size === currentArchives.length) {
      selectedArchives.clear();
    } else {
      currentArchives.forEach((archive) =>
        selectedArchives.add(archive.live_id),
      );
    }
    selectedArchives = selectedArchives; // Trigger reactivity
  }

  async function deleteArchive(archive: RecordItem) {
    try {
      await invoke("delete_archive", {
        platform: archive.platform,
        roomId: archive.room_id,
        liveId: archive.live_id,
      });
      await loadArchives();
      showDeleteConfirm = false;
      archiveToDelete = null;
    } catch (error) {
      console.error("Failed to delete archive:", error);
    }
  }

  async function deleteSelectedArchives() {
    try {
      for (const liveId of selectedArchives) {
        const archive = filteredArchives.find((a) => a.live_id === liveId);
        if (archive) {
          await invoke("delete_archive", {
            platform: archive.platform,
            roomId: archive.room_id,
            liveId: archive.live_id,
          });
        }
      }
      selectedArchives.clear();
      await loadArchives();
      showDeleteConfirm = false;
      archiveToDelete = null;
    } catch (error) {
      console.error("Failed to delete selected archives:", error);
    }
  }

  async function deleteSmallSizeArchives() {
    try {
      const thresholdMb = Number(deleteSizeThresholdMb) || 0;
      const maxSizeBytes = Math.max(0, Math.floor(thresholdMb * 1024 * 1024));
      const deleted = await invoke<number>("delete_small_size_archives", {
        maxSize: maxSizeBytes,
      });
      selectedArchives.clear();
      await loadArchives();
      showDeleteZeroConfirm = false;
      alert(`已删除 ${deleted} 个小于 ${thresholdMb} MB 的录播`);
    } catch (error) {
      console.error("Failed to delete small size archives:", error);
      alert("删除失败");
    }
  }

  async function playArchive(archive: RecordItem) {
    try {
      await invoke("open_live", {
        platform: archive.platform,
        roomId: archive.room_id,
        liveId: archive.live_id,
      });
    } catch (error) {
      console.error("Failed to play archive:", error);
    }
  }

  function openWholeClipModal(archive: RecordItem) {
    wholeClipArchive = archive;
    showWholeClipModal = true;
  }

  function handleWholeClipGenerated() {
    // 生成完成后可以刷新列表或显示通知
    console.log("完整录播生成已开始");
  }
</script>

<div
  class="flex-1 p-6 overflow-auto custom-scrollbar-light bg-gray-50 dark:bg-black"
>
  <div class="space-y-6">
    <!-- Header -->
    <div class="flex justify-between items-center">
      <div class="space-y-1">
        <h1 class="text-2xl font-semibold text-gray-900 dark:text-white">
          录播档案
        </h1>
        <p class="text-sm text-gray-500 dark:text-gray-400">
          管理所有直播间的录播记录，可以查看、播放和管理历史直播内容。
        </p>
      </div>

      <div class="flex items-center gap-3 flex-wrap">
        <span class="text-sm font-medium text-gray-700 dark:text-gray-200 whitespace-nowrap shrink-0">
          删除小于
        </span>
        <div class="relative shrink-0">
          <input
            type="number"
            min="0"
            step="0.1"
            bind:value={deleteSizeThresholdMb}
            class="w-28 pl-3 pr-10 py-2 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-sm text-gray-900 dark:text-white focus:outline-none focus:ring-2 focus:ring-red-500"
          />
          <span
            class="pointer-events-none absolute inset-y-0 right-3 flex items-center text-sm text-gray-600 dark:text-gray-300"
          >
            MB
          </span>
        </div>
        <span class="text-sm font-medium text-gray-700 dark:text-gray-200 whitespace-nowrap shrink-0">
          录播
        </span>
        <button
          class="px-4 py-2 bg-red-500 text-white rounded-lg hover:bg-red-600 transition-colors inline-flex items-center gap-2 whitespace-nowrap shrink-0"
          on:click={() => {
            showDeleteZeroConfirm = true;
          }}
        >
          <Trash2 class="w-4 h-4" />
          <span class="whitespace-nowrap">删除</span>
        </button>
        <button
          class="px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600 transition-colors inline-flex items-center gap-2 whitespace-nowrap shrink-0 disabled:opacity-50 disabled:cursor-not-allowed"
          on:click={loadArchives}
          disabled={loading}
        >
          <RefreshCw
            class="w-4 h-4 text-white {loading ? 'animate-spin' : ''}"
          />
          <span class="whitespace-nowrap">刷新</span>
        </button>
      </div>
    </div>

    <!-- 筛选和排序工具栏 -->
    <div
      class="p-4 rounded-xl bg-white dark:bg-[#3c3c3e] border border-gray-200 dark:border-gray-700 space-y-4"
    >
      <div class="flex justify-between items-center flex-wrap gap-4">
        <!-- 左侧：筛选器和分页 -->
        <div class="flex space-x-3">
          <select
            bind:value={selectedRoomId}
            on:change={applyFilters}
            class="px-3 py-2 bg-gray-100 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 rounded-lg text-gray-900 dark:text-white focus:outline-none focus:ring-2 focus:ring-blue-500 dark:focus:ring-blue-400 cursor-pointer"
          >
            <option value={null}>所有直播间</option>
            {#each roomOptions as option}
              <option value={option.id}>{option.label}</option>
            {/each}
          </select>

          <!-- 分页控制 -->
          {#if totalCount > 0}
            <div
              class="flex items-center space-x-3 px-4 py-2 bg-gray-50 dark:bg-gray-800/50 rounded-lg border border-gray-200 dark:border-gray-600"
            >
              <!-- 记录统计 -->
              <div class="flex items-center space-x-1">
                <span
                  class="text-sm font-medium text-blue-600 dark:text-blue-400"
                >
                  {totalCount}
                </span>
                <span class="text-sm text-gray-500 dark:text-gray-400"
                  >条记录</span
                >
              </div>

              <!-- 分隔线 -->
              <div class="h-4 w-px bg-gray-300 dark:bg-gray-600"></div>

              <!-- 每页大小选择 -->
              <div class="flex items-center space-x-2">
                <span class="text-sm text-gray-600 dark:text-gray-400"
                  >每页</span
                >
                <select
                  bind:value={pageSize}
                  on:change={() => changePageSize(pageSize)}
                  class="px-2 py-1 text-sm bg-white dark:bg-gray-700 border border-gray-300 dark:border-gray-500 rounded-md text-gray-900 dark:text-white focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-blue-500 cursor-pointer min-w-[50px]"
                >
                  {#each pageSizeOptions as size}
                    <option value={size}>{size}</option>
                  {/each}
                </select>
                <span class="text-sm text-gray-600 dark:text-gray-400">条</span>
              </div>

              <!-- 分页导航 -->
              {#if totalPages > 1}
                <!-- 分隔线 -->
                <div class="h-4 w-px bg-gray-300 dark:bg-gray-600"></div>

                <div class="flex items-center space-x-2">
                  <button
                    class="p-1.5 text-gray-500 dark:text-gray-400 hover:text-blue-600 dark:hover:text-blue-400 hover:bg-white dark:hover:bg-gray-700 rounded-md transition-all duration-200 disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:bg-transparent"
                    on:click={prevPage}
                    disabled={currentPage === 1}
                    title="上一页"
                  >
                    <ChevronUp class="w-4 h-4 rotate-[-90deg]" />
                  </button>

                  <div
                    class="flex items-center px-2 py-1 bg-white dark:bg-gray-700 rounded-md border border-gray-200 dark:border-gray-500 min-w-[60px] justify-center"
                  >
                    <span
                      class="text-sm font-medium text-gray-700 dark:text-gray-300"
                    >
                      {currentPage}
                    </span>
                    <span class="text-sm text-gray-400 dark:text-gray-500 mx-1"
                      >/</span
                    >
                    <span class="text-sm text-gray-500 dark:text-gray-400">
                      {totalPages}
                    </span>
                  </div>

                  <button
                    class="p-1.5 text-gray-500 dark:text-gray-400 hover:text-blue-600 dark:hover:text-blue-400 hover:bg-white dark:hover:bg-gray-700 rounded-md transition-all duration-200 disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:bg-transparent"
                    on:click={nextPage}
                    disabled={currentPage === totalPages}
                    title="下一页"
                  >
                    <ChevronDown class="w-4 h-4 rotate-[-90deg]" />
                  </button>
                </div>
              {/if}
            </div>
          {/if}
        </div>

        <!-- 右侧：排序按钮 -->
        <div class="flex items-center space-x-2">
          <span class="text-sm text-gray-600 dark:text-gray-400">排序:</span>
          <button
            class="px-3 py-1.5 text-sm font-medium rounded-lg transition-colors {sortBy ===
            'room_id'
              ? 'bg-blue-500 text-white'
              : 'bg-gray-100 dark:bg-gray-700/50 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}"
            on:click={() => toggleSort("room_id")}
          >
            直播间号
            {#if sortBy === "room_id"}
              {#if sortOrder === "asc"}
                <ChevronUp class="w-3 h-3 inline ml-1" />
              {:else}
                <ChevronDown class="w-3 h-3 inline ml-1" />
              {/if}
            {/if}
          </button>
          <button
            class="px-3 py-1.5 text-sm font-medium rounded-lg transition-colors {sortBy ===
            'title'
              ? 'bg-blue-500 text-white'
              : 'bg-gray-100 dark:bg-gray-700/50 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}"
            on:click={() => toggleSort("title")}
          >
            标题
            {#if sortBy === "title"}
              {#if sortOrder === "asc"}
                <ChevronUp class="w-3 h-3 inline ml-1" />
              {:else}
                <ChevronDown class="w-3 h-3 inline ml-1" />
              {/if}
            {/if}
          </button>
          <button
            class="px-3 py-1.5 text-sm font-medium rounded-lg transition-colors {sortBy ===
            'length'
              ? 'bg-blue-500 text-white'
              : 'bg-gray-100 dark:bg-gray-700/50 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}"
            on:click={() => toggleSort("length")}
          >
            时长
            {#if sortBy === "length"}
              {#if sortOrder === "asc"}
                <ChevronUp class="w-3 h-3 inline ml-1" />
              {:else}
                <ChevronDown class="w-3 h-3 inline ml-1" />
              {/if}
            {/if}
          </button>
          <button
            class="px-3 py-1.5 text-sm font-medium rounded-lg transition-colors {sortBy ===
            'size'
              ? 'bg-blue-500 text-white'
              : 'bg-gray-100 dark:bg-gray-700/50 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}"
            on:click={() => toggleSort("size")}
          >
            大小
            {#if sortBy === "size"}
              {#if sortOrder === "asc"}
                <ChevronUp class="w-3 h-3 inline ml-1" />
              {:else}
                <ChevronDown class="w-3 h-3 inline ml-1" />
              {/if}
            {/if}
          </button>
          <button
            class="px-3 py-1.5 text-sm font-medium rounded-lg transition-colors {sortBy ===
            'created_at'
              ? 'bg-blue-500 text-white'
              : 'bg-gray-100 dark:bg-gray-700/50 text-gray-700 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}"
            on:click={() => toggleSort("created_at")}
          >
            创建时间
            {#if sortBy === "created_at"}
              {#if sortOrder === "asc"}
                <ChevronUp class="w-3 h-3 inline ml-1" />
              {:else}
                <ChevronDown class="w-3 h-3 inline ml-1" />
              {/if}
            {/if}
          </button>
        </div>
      </div>

      <!-- 批量操作栏 -->
      {#if selectedArchives.size > 0}
        <div
          class="flex justify-between items-center pt-4 border-t border-gray-200 dark:border-gray-700"
        >
          <div class="flex items-center space-x-2">
            <div class="w-2 h-2 bg-amber-500 rounded-full"></div>
            <span
              class="text-sm text-amber-700 dark:text-amber-300 font-medium"
            >
              已选择 {selectedArchives.size} 项
            </span>
          </div>
          <button
            class="px-4 py-2 bg-red-600 hover:bg-red-700 text-white rounded-lg transition-colors flex items-center space-x-2"
            on:click={() => {
              showDeleteConfirm = true;
              archiveToDelete = null;
            }}
          >
            <Trash2 class="w-4 h-4" />
            <span>删除选中</span>
          </button>
        </div>
      {/if}
    </div>

    <!-- Archive List -->
    <div
      class="bg-white dark:bg-[#3c3c3e] border border-gray-200 dark:border-gray-700 rounded-xl overflow-hidden"
    >
      {#if loadError}
        <div
          class="flex flex-col items-center justify-center p-12 space-y-4 text-gray-500 dark:text-gray-400"
        >
          <div class="text-red-500 dark:text-red-400 text-lg">加载失败</div>
          <p class="text-sm">{loadError}</p>
          <button
            class="px-4 py-2 bg-blue-500 text-white rounded-lg hover:bg-blue-600 transition-colors"
            on:click={loadArchives}
          >
            重试
          </button>
        </div>
      {:else if loading}
        <div
          class="flex flex-col items-center justify-center p-12 space-y-4 text-gray-500 dark:text-gray-400"
        >
          <RefreshCw class="w-8 h-8 animate-spin" />
          <span>加载录播列表中...</span>
        </div>
      {:else if filteredArchives.length === 0 && !loading}
        <div
          class="flex flex-col items-center justify-center p-12 space-y-4 text-gray-500 dark:text-gray-400"
        >
          <History class="w-12 h-12" />
          <h3 class="text-lg font-medium text-gray-900 dark:text-white">
            暂无录播
          </h3>
          <p class="text-sm">
            {selectedRoomId !== null
              ? "该直播间还没有录播记录"
              : "还没有任何录播记录"}
          </p>
        </div>
      {:else}
        <div class="overflow-x-auto custom-scrollbar-light">
          <table class="w-full">
            <thead>
              <tr class="border-b border-gray-200 dark:border-gray-700/50">
                <th class="px-4 py-3 text-left w-12">
                  <input
                    type="checkbox"
                    checked={selectedArchives.size ===
                      filteredArchives.length && filteredArchives.length > 0}
                    on:change={selectAllArchives}
                    class="rounded border-gray-300 dark:border-gray-600"
                  />
                </th>
                <th
                  class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                  >直播时间</th
                >
                <th
                  class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                  >直播间</th
                >
                <th
                  class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                  >标题</th
                >
                <th
                  class="px-4 py-3 text-left text-sm font-medium text-gray-500 dark:text-gray-400"
                  >分辨率</th
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
              {#each filteredArchives as archive (getArchiveKey(archive))}
                <tr
                  class="group hover:bg-[#f5f5f7] dark:hover:bg-[#3a3a3c] transition-colors"
                >
                  <td class="px-4 py-3">
                    <input
                      type="checkbox"
                      checked={selectedArchives.has(archive.live_id)}
                      on:change={() => toggleArchiveSelection(archive.live_id)}
                      class="rounded border-gray-300 dark:border-gray-600"
                    />
                  </td>

                  <td class="px-4 py-3">
                    <div class="flex flex-col">
                      <span class="text-sm text-gray-900 dark:text-white"
                        >{formatDate(archive.created_at).split(" ")[0]}</span
                      >
                      <span class="text-xs text-gray-500 dark:text-gray-400"
                        >{formatDate(archive.created_at).split(" ")[1]}</span
                      >
                    </div>
                  </td>

                  <td class="px-4 py-3">
                    <div class="flex items-center space-x-2">
                      {#if archive.platform === "bilibili"}
                        <BilibiliIcon class="w-4 h-4" />
                      {:else if archive.platform === "douyin"}
                        <DouyinIcon class="w-4 h-4" />
                      {:else if archive.platform === "kuaishou"}
                        <KuaishouIcon class="w-4 h-4" />
                      {:else if archive.platform === "huya"}
                        <HuyaIcon class="w-4 h-4" />
                      {:else if archive.platform === "tiktok"}
                        <TikTokIcon class="w-5 h-5" />
                      {:else}
                        <Globe class="w-4 h-4 text-gray-400" />
                      {/if}
                      {#if getRoomUrl(archive.platform, archive.room_id)}
                        <a
                          href={getRoomUrl(archive.platform, archive.room_id)}
                          target="_blank"
                          rel="noopener noreferrer"
                          class="text-blue-500 hover:text-blue-700 text-sm"
                          title={`打开 ${formatPlatform(archive.platform)} 直播间`}
                        >
                          {archive.room_id}
                        </a>
                      {:else}
                        <span class="text-sm text-gray-900 dark:text-white"
                          >{archive.room_id}</span
                        >
                      {/if}
                    </div>
                  </td>

                  <td class="px-4 py-3">
                    <div class="flex items-center space-x-3">
                      {#if archive.cover}
                        <img
                          src={archive.cover}
                          alt="封面"
                          class="w-12 h-8 rounded object-cover flex-shrink-0"
                          on:load={(e) => handle_archive_cover_load(e, archive)}
                          on:error={(e) =>
                            handle_archive_cover_error(e, archive)}
                        />
                      {/if}
                      <span
                        class="text-sm text-gray-900 dark:text-white truncate"
                        >{archive.title}</span
                      >
                    </div>
                  </td>

                  <td class="px-4 py-3">
                    <span class="text-sm text-gray-600 dark:text-gray-300"
                      >{archive.resolution || "-"}</span
                    >
                  </td>

                  <td class="px-4 py-3">
                    <div class="flex items-center space-x-2">
                      <Clock class="w-4 h-4 text-gray-400" />
                      <span class="text-sm text-gray-900 dark:text-white"
                        >{formatDuration(archive.length)}</span
                      >
                    </div>
                  </td>

                  <td class="px-4 py-3">
                    <div class="flex items-center space-x-2">
                      <HardDrive class="w-4 h-4 text-gray-400" />
                      <span class="text-sm text-gray-900 dark:text-white"
                        >{formatSize(archive.size)}</span
                      >
                    </div>
                  </td>

                  <td class="px-4 py-3">
                    <span class="text-sm text-gray-500 dark:text-gray-400"
                      >{calcBitrate(archive.size, archive.length)} Kbps</span
                    >
                  </td>

                  <td class="px-4 py-3">
                    <div class="flex items-center space-x-2">
                      <button
                        class="p-1.5 rounded-lg hover:bg-blue-500/10 transition-colors"
                        title="预览录播"
                        on:click={() => playArchive(archive)}
                      >
                        <Play class="w-4 h-4 text-blue-500" />
                      </button>
                      <button
                        class="p-1.5 rounded-lg hover:bg-blue-500/10 transition-colors"
                        title="生成完整切片"
                        on:click={() => openWholeClipModal(archive)}
                      >
                        <FileVideo class="w-4 h-4 text-blue-500" />
                      </button>
                      <button
                        class="p-1.5 rounded-lg hover:bg-red-500/10 transition-colors"
                        title="删除记录"
                        on:click={() => {
                          archiveToDelete = archive;
                          showDeleteConfirm = true;
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
      {/if}
    </div>
  </div>
</div>

<!-- Delete Confirmation Modal -->
{#if showDeleteConfirm || showDeleteZeroConfirm}
  <div
    class="fixed inset-0 bg-black/20 dark:bg-black/40 backdrop-blur-sm z-50 flex items-center justify-center"
  >
    <div
      class="mac-modal w-[400px] bg-white dark:bg-[#323234] rounded-xl shadow-xl overflow-hidden"
    >
      <div class="p-6 space-y-4">
        <div class="text-center space-y-2">
          <h3 class="text-base font-medium text-gray-900 dark:text-white">
            确认删除
          </h3>
          <p class="text-sm text-gray-500 dark:text-gray-400">
            {#if showDeleteZeroConfirm}
              确定要删除小于 ${deleteSizeThresholdMb} MB 的录播吗？
            {:else if archiveToDelete}
              确定要删除录播 "{archiveToDelete.title}" 吗？
            {:else}
              确定要删除选中的 {selectedArchives.size} 个录播吗？
            {/if}
          </p>
          <p class="text-xs text-red-600 dark:text-red-500">此操作无法撤销。</p>
        </div>
        <div class="flex justify-center space-x-3">
          <button
            class="w-24 px-4 py-2 text-sm font-medium text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-600 rounded-lg transition-colors"
            on:click={() => {
              showDeleteConfirm = false;
              showDeleteZeroConfirm = false;
              archiveToDelete = null;
            }}
          >
            取消
          </button>
          <button
            class="w-24 px-4 py-2 bg-red-600 hover:bg-red-700 text-white text-sm font-medium rounded-lg transition-colors"
            on:click={() => {
              if (showDeleteZeroConfirm) {
                deleteSmallSizeArchives();
              } else if (archiveToDelete) {
                deleteArchive(archiveToDelete);
              } else {
                deleteSelectedArchives();
              }
            }}
          >
            删除
          </button>
        </div>
      </div>
    </div>
  </div>
{/if}

<GenerateWholeClipModal
  bind:showModal={showWholeClipModal}
  archive={wholeClipArchive}
  roomId={wholeClipArchive?.room_id || ""}
  platform={wholeClipArchive?.platform || ""}
  on:generated={() => {
    loadArchives();
  }}
/>
