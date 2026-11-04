# GitHub Actions 工作流说明

本项目包含多个 GitHub Actions 工作流，用于自动化构建和发布流程。

## 工作流列表

### 1. Release (`main.yml`)

**触发条件：**
- 推送标签（格式：`v*`，例如 `v2.17.7`）
- 手动触发（workflow_dispatch）

**功能：**
- 为多个平台构建应用程序：
  - macOS (Intel x86_64)
  - macOS (Apple Silicon aarch64) ✨ 新增
  - Ubuntu 22.04
  - Windows (CPU 版本)
  - Windows (CUDA 版本)
- 自动创建 GitHub Release（草稿模式）
- 上传构建产物到 Release

**使用方法：**
```bash
# 创建并推送标签
git tag v2.17.7
git push origin v2.17.7
```

### 2. Build DMG (`build-dmg.yml`) ✨ 新增

**触发条件：**
- 手动触发（可选择构建目标）
- 推送到 `main` 或 `develop` 分支（当 `src-tauri/` 或 `src/` 目录有变更时）

**功能：**
- 专门用于构建 macOS DMG 安装包
- 支持三种构建目标：
  - `aarch64-apple-darwin` - Apple Silicon (M1/M2/M3)
  - `x86_64-apple-darwin` - Intel Mac
  - `universal` - 通用二进制（同时支持 Apple Silicon 和 Intel）

**使用方法：**

#### 手动触发构建：
1. 访问 GitHub 仓库的 Actions 页面
2. 选择 "Build DMG" 工作流
3. 点击 "Run workflow"
4. 选择构建目标：
   - `aarch64-apple-darwin` - 仅构建 Apple Silicon 版本（推荐用于 M 系列芯片）
   - `x86_64-apple-darwin` - 仅构建 Intel 版本
   - `universal` - 构建通用版本（文件较大，但兼容所有 Mac）
5. 点击 "Run workflow" 开始构建

#### 自动触发：
当你推送代码到 `main` 或 `develop` 分支，且修改了以下目录时会自动触发：
- `src-tauri/**`
- `src/**`

**构建产物：**
- DMG 文件会作为 Artifacts 上传，可在 Actions 运行页面下载
- 文件命名格式：`LIVE_{version}_{arch}.dmg`
  - 例如：`LIVE_2.17.7_aarch64.dmg`

### 3. Docker Build and Push (`package.yml`)

**触发条件：**
- 推送标签（格式：`v*`）
- 手动触发

**功能：**
- 构建 Docker 镜像
- 推送到 GitHub Container Registry (ghcr.io)

## 构建目标说明

### macOS 架构选择指南

| Mac 型号 | 推荐构建目标 | 说明 |
|---------|------------|------|
| M1/M2/M3 Mac | `aarch64-apple-darwin` | 原生性能最佳 |
| Intel Mac | `x86_64-apple-darwin` | 原生性能最佳 |
| 需要兼容所有 Mac | `universal` | 文件较大，但通用 |

### 文件大小对比

- Apple Silicon 版本：~129 MB
- Intel 版本：~129 MB
- Universal 版本：~258 MB（包含两个架构）

## 常见问题

### Q: 如何只构建 Apple Silicon 版本？
A: 在 "Build DMG" 工作流中选择 `aarch64-apple-darwin` 目标。

### Q: 构建失败怎么办？
A: 
1. 检查 Actions 日志中的错误信息
2. 确保 `src-tauri/Cargo.toml` 中的版本号正确
3. 确保所有依赖都已正确配置

### Q: 如何下载构建的 DMG 文件？
A:
1. 进入 Actions 页面
2. 点击对应的工作流运行
3. 在 "Artifacts" 部分下载 DMG 文件

### Q: 为什么 DMG 文件名包含 `rw.` 前缀？
A: 这些是 Tauri 构建过程中的临时文件。工作流会自动将它们重命名为正确的格式。

## 本地构建命令

如果你想在本地构建 DMG：

```bash
# Apple Silicon
yarn tauri build --target aarch64-apple-darwin

# Intel
yarn tauri build --target x86_64-apple-darwin

# 或者直接使用（会根据当前系统架构构建）
yarn tauri build
```

## 环境变量

工作流使用以下环境变量：

- `CMAKE_OSX_DEPLOYMENT_TARGET`: macOS 最低支持版本（13.3）
- `CMAKE_CUDA_ARCHITECTURES`: CUDA 架构版本（仅 Windows CUDA 构建）
- `WHISPER_BACKEND`: Whisper 后端类型（cpu/cuda）

## 更新日志

### 2026-01-28
- ✨ 添加 Apple Silicon (aarch64) 支持到主 Release 工作流
- ✨ 创建专门的 DMG 构建工作流 (`build-dmg.yml`)
- ✨ 支持 Universal 二进制构建
- 🐛 修复数据库迁移问题（移除重复的迁移15）
