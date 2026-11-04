# PrismReplay 构建指南

## 快速开始

### 本地开发
```bash
# 安装依赖
yarn install

# 开发模式
yarn tauri dev

# 清理旧产物
yarn clean

# 构建 Apple Silicon DMG（自动完成 .app + .dmg）
yarn build:mac:arm64

# 构建 Intel DMG（自动完成 .app + .dmg）
yarn build:mac:x64
```

### GitHub Actions 自动构建

#### 方式 1: 手动触发 DMG 构建
1. 访问 [Actions](../../actions/workflows/build-dmg.yml) 页面
2. 点击 "Run workflow"
3. 选择构建目标：
   - `aarch64-apple-darwin` - Apple Silicon (M1/M2/M3)
   - `x86_64-apple-darwin` - Intel Mac
   - `universal` - 通用版本
4. 等待构建完成，下载 Artifacts

#### 方式 2: 发布新版本
```bash
# 更新版本号（在 src-tauri/Cargo.toml）
# version = "2.17.8"

# 提交更改
git add .
git commit -m "chore: bump version to 2.17.8"

# 创建标签并推送
git tag v2.17.8
git push origin main
git push origin v2.17.8
```

## 构建目标说明

| 目标 | 适用设备 | 文件大小 |
|-----|---------|---------|
| aarch64-apple-darwin | M1/M2/M3 Mac | ~129 MB |
| x86_64-apple-darwin | Intel Mac | ~129 MB |
| universal | 所有 Mac | ~258 MB |

## 更多信息

详细的工作流说明请查看 [.github/WORKFLOWS.md](.github/WORKFLOWS.md)

## 故障排除

### 数据库迁移错误
如果遇到迁移错误，删除数据库文件：
```bash
rm -f ~/Library/Application\ Support/cn.PrismReplay/data_v2.db
```

### 构建失败
1. 确保 Rust 工具链已安装
2. 确保 Node.js 版本为 LTS
3. macOS 上建议显式设置 SDK 环境变量：
```bash
export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export CMAKE_OSX_DEPLOYMENT_TARGET=13.3
```
4. 清理并重新构建：
```bash
yarn clean
yarn install
yarn build:mac:arm64
```
5. 本仓库已改为自动使用 `hdiutil` 封装 DMG，不再依赖 Tauri 自带的 `bundle_dmg.sh`。
