# 打包、安装与发布

本项目仍然只是支付网关的聚合服务，不提供支付通道、扣款、资金托管或结算。打包流程不会创建商户、注册真实回调或自动启用收款服务。

## 一个构建入口

```sh
make check       # Rust 校验 + 打包脚本单元测试
make build       # 当前 CPU 架构的静态 musl 可执行文件
make package     # 相同二进制 → tar.gz、deb、rpm、构建清单、校验和
make image       # 相同已验证二进制 → 本地 pay-lmm:local 镜像
```

构建主机要求 Linux、Python 3.11+、Rust（版本固定在 `rust-toolchain.toml`）、C 编译器、musl-gcc、protoc。生成原生包还需要 dpkg-deb、rpmbuild、xz；Docker 只用于镜像与跨发行版验证。Rust、Python、protoc 和打包工具**不需要安装到运行服务器**。

在 Debian/Ubuntu 构建主机安装构建依赖：

```sh
sudo apt-get update
sudo apt-get install build-essential musl-tools protobuf-compiler openssl python3 rpm dpkg-dev xz-utils
make package
```

`TARGET=x86_64-unknown-linux-musl` 和 `TARGET=aarch64-unknown-linux-musl` 分别在对应 CPU 架构的主机运行。构建脚本会拒绝宿主架构不匹配，而不是悄悄生成错误架构文件。CI 使用原生 x86_64 / ARM64 runner，不使用 QEMU 编译。

普通 `cargo build --release` 仍可用于本机开发，但 GNU 主机上生成的二进制**不是标准发行产物**。请勿把它重命名为通用 Linux 包。

## 产物与兼容边界

以应用版本 `0.1.0` 为命名示例，实际版本来自 Cargo.toml：

| 产物 | amd64 示例 | arm64 区别 |
| --- | --- | --- |
| 通用压缩包 | `pay-lmm-v0.1.0-linux-amd64.tar.gz` | 文件名 `arm64` |
| Debian 包 | `pay-lmm_0.1.0-1_amd64.deb` | Debian 架构 `arm64` |
| RPM 包 | `pay-lmm-0.1.0-1.x86_64.rpm` | RPM 架构 `aarch64` |
| 构建清单 | `pay-lmm-v0.1.0-linux-amd64.build.json` | 文件名 `arm64` |
| 汇总校验和 | `SHA256SUMS` | 包含两个架构的全部标准文件 |
| 容器镜像 | `ghcr.io/tokennotincluded/pay.lmm.best:v0.1.0` | 同一标签中的 `linux/arm64` manifest |

镜像标签仅在对应版本标签成功发布后存在。普通分支提交只生成 CI artifacts，**不能据此认为 GitHub Release 或 GHCR 镜像已公开发布**。

标准二进制静态链接 musl；打包脚本直接解析 ELF，检查架构、禁止 `PT_INTERP` 动态解释器和 `DT_NEEDED` 动态库依赖。不是只根据文件名判断兼容性。因此运行产物不依赖宿主 glibc 的具体版本，也不需要宿主 OpenSSL 动态库、Node、Python 或 Rust 运行时。

CI 发行版矩阵：

| 用户空间镜像 | 安装格式 | 架构 |
| --- | --- | --- |
| Debian 12 | deb | amd64、arm64 |
| Ubuntu 22.04 | deb | amd64、arm64 |
| Fedora 43 | rpm | amd64、arm64 |
| Rocky Linux 9 | rpm | amd64、arm64 |
| openSUSE Leap 16.0 | rpm | amd64、arm64 |
| Alpine 3.22 | tar.gz + OpenRC | amd64、arm64 |
| Arch Linux base | tar.gz + systemd 单元 | amd64 |

这些是发行版**用户空间容器测试**，共 13 个组合，不是 13 台完整启动的虚拟机。它们共用 runner 内核；没有由此验证旧内核、所有衍生发行版、32 位系统、Windows/macOS、真实 systemd/OpenRC 开机过程或特殊安全策略。Arch ARM 没有纳入此矩阵。提供 tar.gz 并不等于已经发布 APK、Arch 原生包、AUR、APT/YUM 仓库。

## 安装前检查

从成功的 CI `release-assets` artifact 或相应版本 Release 获取产物。保留完整资产集合时先运行：

```sh
sha256sum -c SHA256SUMS
```

仅下载一个文件时，用可信渠道获得的 SHA256SUMS 中**该文件对应行**校验它；不要忽略校验失败。哈希防止内容损坏或替换，但 SHA256SUMS 本身不是发布者数字签名。本版没有宣称提供 GPG 签名、Sigstore 签名或经过认证的软件供应链证明。构建清单用于追溯，不是不可伪造的证明。

### Debian / Ubuntu

```sh
sudo apt install ./pay-lmm_0.1.0-1_amd64.deb
```

### Fedora / Rocky / openSUSE

使用自己系统的原生包管理器安装相应 RPM，例如：

```sh
sudo dnf install ./pay-lmm-0.1.0-1.x86_64.rpm
# openSUSE 使用 zypper install ./对应文件.rpm
```

RPM 尚未附带 GPG 发布签名；需要遵守部署环境对本地软件包的信任策略，不要全局关闭仓库签名检查。

原生包安装 `/usr/bin/pay-lmm` 与 `/usr/lib/systemd/system/pay-lmm.service`，创建专用系统用户和必要目录，但不会启动、启用或重启服务。账户创建工具是包管理依赖；应用本身是静态二进制。

### 通用 tar.gz / Alpine / Arch

```sh
tar -xzf pay-lmm-v0.1.0-linux-amd64.tar.gz
cd pay-lmm-v0.1.0-linux-amd64
sh install.sh --prefix "$HOME/.local"   # 非 root，仅安装程序及文档
# 或显式安装为系统服务：
sudo sh install.sh --system            # /usr/local/bin，仍不自动启动
```

安装器完全离线，不执行 curl|sh、不自动提权、不下载依赖。它在执行或替换二进制前验证包内 SHA256SUMS 和架构，通过临时文件原子替换可执行文件。`--system` 自动选择已安装的 OpenRC 或 systemd；未检测到管理器时只安装程序并建立目录。

`--system` 会拒绝覆盖未由本安装器管理的管理员单元，也拒绝与 `/usr/bin/pay-lmm` 原生安装共存。非 root 安装不会创建系统账户和服务，需要自行使用 `--config` 指定配置。

卸载已由归档安装器管理的程序：

```sh
sh install.sh --prefix "$HOME/.local" --uninstall
# 系统归档安装对应：sudo sh install.sh --system --uninstall
```

原生包请使用 apt/dnf/zypper 卸载，不要用归档安装器管理它。

## 配置与服务管理

所有系统安装使用统一位置：

| 路径 | 用途 / 权限 |
| --- | --- |
| `/etc/pay.lmm.best/` | root:pay-lmm，0750 |
| `/etc/pay.lmm.best/config.toml` | 管理员创建，建议 root:pay-lmm，0640 |
| `/etc/pay.lmm.best/secrets.env` | 管理员创建，root:root，0600 |
| `/var/lib/pay-lmm/` | pay-lmm:pay-lmm，0700；持久化数据 |
| `/var/lib/pay-lmm/pay.sqlite3` | 建议数据库位置，由进程创建 |

原生安装的示例位于 `/usr/share/doc/pay-lmm/`；归档系统安装位于 `/usr/local/share/doc/pay-lmm/`。包内示例的数据库路径已规范化为绝对路径。下面仅复制示例，不填入任何真实支付凭据：

```sh
sudo install -m 0640 -o root -g pay-lmm /usr/share/doc/pay-lmm/examples/config.toml /etc/pay.lmm.best/config.toml
sudo install -m 0600 -o root -g root /usr/share/doc/pay-lmm/deploy/secrets.env.example /etc/pay.lmm.best/secrets.env
```

归档安装相应替换文档前缀为 `/usr/local`。编辑真实上游地址、商户配置及环境变量后，完成自己的上游沙箱验收，再启用服务。凭据文件只使用 `NAME='value'` 赋值，不含 export、命令或命令替换；RSA PEM 可使用应用支持的字面 `\n`。OpenRC 会作为 root 读取这些受保护的赋值，不能让非管理员修改它。

```sh
# systemd
sudo systemctl daemon-reload
sudo systemctl enable --now pay-lmm
sudo journalctl -u pay-lmm -f

# OpenRC：仅在确认配置后运行
sudo rc-update add pay-lmm default
sudo rc-service pay-lmm start
```

OpenRC 的 stdout/stderr 在 `/var/lib/pay-lmm/`，应配置日志轮转。systemd 使用 journal。TLS 与连接限流由前置代理负责，可参考 `deploy/Caddyfile`。

### 升级与旧部署迁移

包升级不覆盖、生成、删除运行配置、密钥和数据库，也不重新生成用户身份。重新安装、升级到包修订 2、卸载/purge 后的文件保留均由测试检查。程序删除时只停止托管服务并移除程序，保留用户、配置及支付证据，避免 UID 复用或幂等数据丢失。

**升级不自动重启进程。** 先做好一致的数据库备份，安装新包，检查变更后手动 restart。新进程启动可能执行数据库迁移；回滚程序前需确认旧版本能读取新 schema。只降级二进制不等于完整数据回滚。

旧手工部署可能同时存在 `/usr/local/bin/pay-lmm` 和 `/etc/systemd/system/pay-lmm.service`。后者会覆盖发行包的 vendor unit。迁移前检查 `type -a pay-lmm`、`systemctl cat pay-lmm`，停止旧实例、备份配置与数据库，确认最终使用的二进制和数据路径。包不会擅自删除管理员旧单元，也不会搬移原数据库。SQLite 不允许两个实例共享同一数据库。

## 容器

Dockerfile 不再重新编译另一份程序。先 `make package` 再 `docker build`，或者直接 `make image`；容器使用同一份通过 ELF/测试校验的二进制。runtime 为 scratch，无 shell，无包管理器，默认用户 `10001:10001`。

`deploy/compose.yaml` 默认引用本地 `pay-lmm:local`，不假设远程标签已经发布。容器配置需把监听改成 `0.0.0.0:8080`，数据库设为 `/var/lib/pay-lmm/pay.sqlite3`。named volume 提供持久化，bind mount 的数据目录则需管理员事先赋予 UID/GID 10001 的写权限。根文件系统可只读，并为 `/tmp` 提供 tmpfs。

不要将 `--version` 当作就绪探针；从容器外调用 `/readyz`。scratch 中没有 curl/sh，不能照搬依赖 shell 的 HEALTHCHECK。容器测试以默认非 root 用户、只读根目录、无额外 capabilities 运行并验证真实 SQLite 操作和签名回调。

## 发布门禁

版本源为 Cargo.toml；Rust 编译器版本固定，Cargo.lock 提交入库，使用 `--locked`。稳定版本标签为 `vX.Y.Z`，预发布为 `vX.Y.Z-rc.1` 等；原生包把预发布连字符转换为 `~` 保持排序，文件名保留原 SemVer。不支持 SemVer `+build` 元数据。

`Packages` 工作流在 PR、main、打包工作分支和手动触发时构建与验证；只在匹配 Cargo.toml 的版本标签上发布：

1. 两个原生架构分别完成静态构建、Rust 测试、Clippy、打包单元测试、文件格式和内存 smoke 检查。
2. 从同一二进制生成所有格式；通用归档固定文件顺序、属主、时间戳和 gzip header，并实际生成两次比较哈希。deb/rpm 设置 SOURCE_DATE_EPOCH，但没有承诺所有工具链/主机上的逐字节重现。
3. 13 个用户空间组合验证安装、非 root 64 笔模拟订单流程、重装、deb/rpm 包修订升级、卸载和配置/SQLite 保留。
4. 汇总时校验每个资产、两个 CPU 架构、完整文件集合、源提交、版本、干净工作区和镜像中的二进制哈希。
5. 标签提交必须属于 main 的历史，且同名 Release 不存在。之后推送双架构 GHCR 镜像，建立草稿 Release 并上传资产，最后公开；失败会保留为失败/草稿状态，不覆盖已发布资产。

普通构建任务只有仓库读权限；只有标签发布任务获得 contents/packages 写权限。没有 CI 自动修改源码的初始化任务。镜像只生成精确版本标签，不自动移动 `latest`，避免无意升级生产环境。

构建清单含源码 SHA、Rust/Cargo 版本、架构、Cargo.lock/构建脚本/二进制哈希和大小。归档与原生包附带自动收集的依赖声明和许可证文本；这不是独立许可证审计。系统构建工具和发行版测试镜像仍会接收发行方更新，不能把本流程称为完全密封或无网络构建。

发布流程搭建完成本身不会创建版本标签、公开 Release、推送 GHCR 镜像或部署服务；这些动作需要后续显式推送版本标签触发。

## 参考

- Rust 静态 C runtime 与检查生成产物：[Rust Reference](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)。
- 原生包生命周期：[Debian Policy](https://www.debian.org/doc/debian-policy/ch-maintainerscripts.html)、[RPM spec](https://rpm.org/docs/4.20.x/manual/spec.html)。
- 多平台镜像机制：[Docker documentation](https://docs.docker.com/build/building/multi-platform/)。
