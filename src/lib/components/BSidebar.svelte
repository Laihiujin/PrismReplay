<script>
  import {
    FileVideo,
    LayoutDashboard,
    List,
    Settings,
    Users,
    Video,
    Brain,
    History,
  } from "lucide-svelte";
  import SidebarItem from "./SidebarItem.svelte";
  import { createEventDispatcher } from "svelte";

  const dispatch = createEventDispatcher();

  export let activeUrl = "overview";

  /** @param {{ detail: string }} route */
  function navigate(route) {
    dispatch("activeChange", route.detail);
  }
</script>

<div
  class="w-48 bg-[#f0f0f3]/50 dark:bg-black backdrop-blur-xl border-r border-gray-200 dark:border-gray-700"
>
  <div class="flex items-center gap-2 px-4 pt-4 pb-2">
    <img src="/app-icon.png" alt="LIVE" class="w-10 h-10 object-contain" />
    <span class="text-base font-semibold text-gray-900 dark:text-white">LIVE</span>
  </div>
  <nav class="p-3 space-y-1">
    <SidebarItem label="总览" value="overview" {activeUrl} on:activeChange={navigate}>
      <div slot="icon">
        <LayoutDashboard class="w-5 h-5" />
      </div>
    </SidebarItem>
    <SidebarItem label="直播间" value="room" {activeUrl} on:activeChange={navigate}>
      <div slot="icon">
        <Video class="w-5 h-5" />
      </div>
    </SidebarItem>
    <SidebarItem label="录播" value="archive" {activeUrl} on:activeChange={navigate}>
      <div slot="icon">
        <History class="w-5 h-5" />
      </div>
    </SidebarItem>
    <SidebarItem label="切片" value="clip" {activeUrl} on:activeChange={navigate}>
      <div slot="icon">
        <FileVideo class="w-5 h-5" />
      </div>
    </SidebarItem>
    <SidebarItem label="任务" value="task" {activeUrl} on:activeChange={navigate}>
      <div slot="icon">
        <List class="w-5 h-5" />
      </div>
    </SidebarItem>
    <SidebarItem label="助手" value="ai" {activeUrl} on:activeChange={navigate}>
      <div slot="icon">
        <Brain class="w-5 h-5" />
      </div>
    </SidebarItem>
    <SidebarItem label="账号" value="account" {activeUrl} on:activeChange={navigate}>
      <div slot="icon">
        <Users class="w-5 h-5" />
      </div>
    </SidebarItem>
    <SidebarItem label="设置" value="setting" {activeUrl} on:activeChange={navigate}>
      <div slot="icon">
        <Settings class="w-5 h-5" />
      </div>
    </SidebarItem>
  </nav>
</div>

<style>
  :global(img.text-\[\#0A84FF\]) {
    filter: invert(48%) sepia(85%) saturate(2229%) hue-rotate(198deg)
      brightness(100%) contrast(101%);
  }

  :global(img.text-gray-700) {
    filter: invert(23%) sepia(10%) saturate(532%) hue-rotate(182deg)
      brightness(94%) contrast(90%);
  }

  :global(.dark img.text-\[\#0A84FF\]) {
    filter: invert(48%) sepia(85%) saturate(2229%) hue-rotate(198deg)
      brightness(100%) contrast(101%);
  }
</style>
