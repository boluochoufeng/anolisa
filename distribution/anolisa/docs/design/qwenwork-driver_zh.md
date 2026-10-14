# 千问办公桌面适配器

[English](qwenwork-driver.md)

`qwenwork` driver 管理千问办公 QwenWorkCN macOS 本地会话使用的 Tokenless 旁加载文件，
不管理桌面应用安装或激活。驱动沿用 AdapterManager 的锁、收据、完整性检查和日志边界，
不执行随包生命周期脚本。

## 收据与事务

`DriverPayload::QwenWork` 引用选定数据目录下的两个已验证外部资源：
`plugins-custom/tokenless` 插件目录和 `.anolisa-qwenwork-tokenless` 恢复槽。
变更前先在收据中记录授权完整目录树的清单摘要。已有收据格式不变；不识别新载荷的旧版
CLI 无法管理这些收据。降级到此类 CLI 或切回独立脚本前，先完成 disable。

保持与 Tokenless 独立安装器的归属标记兼容。清单包含相对路径、文件摘要、权限和目录。
首次 enable 可接管标记验证通过的脚本安装；再次 enable 还会核对原收据，因此重写标记
不能静默改变已有归属。复制文件必须匹配包管理清单；源 Bundle 必须已包含自包含 Hook，
不能保留源码 Checkout 的共享目录符号链接。

Manager 的 `reconcile_qwenwork` 操作检查两个目标路径和替换源的边界。在应用插件扫描
目录之外完成暂存和验证，首次安装使用不覆盖 rename，替换使用原子交换。收据初始状态为
`cleanup_failed`，直到发布和清理均成功才移除旧摘要并标记完成。

恢复仅接受收据授权的清单。删除时最后移除归属标记，部分删除的恢复目录允许保留原清单
中仍匹配的子集。删除标记与移除目录之间中断时，可清理空恢复槽。新增或修改内容会阻止
清理。进程突然终止留下的未发布临时副本位于扫描目录之外，后续操作不会自动接管或递归
清理它。

## 状态与兼容性

检测直接读取应用 plist，不启动框架 CLI。enable 通过有超时限制的 Python 版本预检并
检查所需二进制。1.2.5 是 Tokenless 已有验证目标，其他版本输出提示。应用和数据目录
环境变量与 Tokenless 一致；修改数据目录配置后须恢复原配置才能清理已有收据。

`TreePresent` 表示复制目录树的完整性；Manager 补充源版本和包完整性条件。
`ActivationEnabled`、`PluginResourcesLoaded` 保持 `unknown`，完整的旁加载仍汇总为
`unknown`。CLI 输出 `Sideloaded` 和界面确认／重启提示，JSON 保留这些区别。
disable 不依赖桌面应用或原始组件 Bundle 仍然存在。

## 验证边界

Rust 测试覆盖 plist 格式、平台拒绝、与现有 Python 安装器的标记兼容、接管、漂移保护、
替换和删除中断、收据序列化及 Manager 路径边界。CLI 集成测试在 Linux 验证拒绝变更，
在 macOS 验证完整流程。这些测试使用夹具 Bundle；实际桌面加载和 Hook 行为需通过
macOS 千问办公会话验证，文件系统测试不能证明这些行为。
