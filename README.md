<p align="center">
  <h1 align="center">RWaveAnalyzer</h1>
  <p align="center">
    一个快速的单文件命令行工具，用来查看 RTL 仿真波形。
    支持 <b>VCD</b>、<b>FST</b>、<b>GHW</b>，并实验性支持 <b>WLF</b> 和 <b>FSDB</b>，
    面向 RTL 调试、CI 和 AI agent。
  </p>
</p>

<p align="center">
  <img alt="Release" src="https://img.shields.io/github/v/release/neveltyc/RWaveAnalyzer?sort=semver&style=flat-square&color=3366cc">
  <img alt="CI" src="https://img.shields.io/github/actions/workflow/status/neveltyc/RWaveAnalyzer/ci.yml?branch=main&style=flat-square&label=CI">
  <img alt="License" src="https://img.shields.io/badge/license-MIT-3366cc?style=flat-square">
</p>

<p align="center">
  <a href="README_en.md">English</a> · <b>中文</b>
</p>

---

## 为什么用 RWaveAnalyzer？

假设你手上有一份通宵回归跑出来的 FST，好几个 GB，你想知道 `arvalid` 和
`arready` 到底什么时候同时为高，或者 `state[3:0]` 在 17.55 µs 时的值是多少。用
Verdi 或 GTKWave 的话，得等 GUI 启动，在层级里一层层点开，再对着光标读取数值。
RWaveAnalyzer 在终端里一条命令就能回答：

```sh
rwave search sim.fst --condition "arvalid=1,arready=1" --show araddr,arlen
```

rwave 本身就是一个二进制文件，不依赖别的东西。它能读取 VCD、FST、GHW 这几种开放
格式；在 linux-amd64 上还能读取 WLF（Mentor/Questa）和 FSDB（Synopsys/Verdi）
数据库，靠的是调用各家自己的库（见 [WLF 和 FSDB](#wlf-与-fsdb-实验性支持)），属于
实验性支持。每条命令都能用 `--json` 输出，字段名是固定的，所以人在终端里能用，
写进 CI 脚本能用，AI agent 也能用。处理整个文件的命令都是流式的，占用内存有上
限，因此哪怕 dump 里有几十万个信号，也不会耗尽内存。

## 快速上手

随便哪条命令，后面跟上一个 `.vcd`、`.fst`、`.ghw`（或 `.wlf`、`.fsdb`）文件就行：

```sh
# 这个文件里有什么？
rwave info sim.fst

# 看看时钟和复位
rwave list sim.fst --filter clk,rst

# 100 ns 到 200 ns 之间发生了什么？
rwave dump sim.fst --begin 100ns --end 200ns --filter state

# valid 和 ready 什么时候同时为高？
rwave search sim.fst --condition "valid=1,ready=1" --show data

# req 在 ready 为低时翻转的那些时刻？
rwave search sim.fst --condition "changed(req),ready=0" --show state

# 多个通道里任意一个握手的时刻？（重复 --condition 就是 OR）
rwave search sim.fst --condition "ch0_valid=1,ch0_ready=1" --condition "ch1_valid=1,ch1_ready=1"

# 恰好在 17.55 us 时，所有已知值是多少？
rwave snapshot sim.fst --at 17.55us --filter state,init_done

# 两个时刻之间有哪些变化？
rwave compare sim.fst --at 17.5us,17.7us --filter bus

# 哪些信号有活动，哪些是静态的？
rwave summary sim.fst --filter alu

# 只看这个模块，不包含它的子模块
rwave list sim.fst --scope u_tx.u_fifo --depth 1

# 除了 CDC 同步器以外的一切
rwave summary sim.fst --exclude '*_sync_*.*'
```

任何命令加上 `--json`，输出就变成紧凑的、方便程序读取的格式。

## 安装

从[最新 release](https://github.com/neveltyc/RWaveAnalyzer/releases/latest)
下载对应平台的 `rwave` 二进制：

| 平台 | 二进制 | VCD · FST · GHW | WLF | FSDB |
|:--|:--|:--:|:--:|:--:|
| Linux x86-64          | `rwave-linux-amd64`       | ✓ | ✓ | ✓ |
| Linux ARM64           | `rwave-linux-arm64`       | ✓ | — | — |
| Windows x86-64        | `rwave-windows-amd64.exe` | ✓ | — | — |
| macOS (Apple Silicon) | `rwave-macos-arm64`       | ✓ | — | — |

```sh
curl -fsSL -o rwave \
  https://github.com/neveltyc/RWaveAnalyzer/releases/latest/download/rwave-linux-amd64
chmod +x rwave
./rwave --version
```

每个二进制都能读取 VCD、FST、GHW；WLF 和 FSDB 只有 linux-amd64 支持（见
[WLF 和 FSDB](#wlf-与-fsdb-实验性支持)）。`rwave-linux-amd64` 动态链接 glibc，
基线是 2.17（manylinux2014），所以 2014 年以后的主流 Linux 发行版都能运行。

## 从源码构建

本地构建只需要一个较新的稳定版 Rust 工具链（基于 1.90、edition 2024 开发）。
整个构建是纯 Rust 的，没有 C 代码，没有 `build.rs`，也不用安装任何系统依赖，所以
一条普通的 `cargo` 命令就能编译出当前机器的二进制：

```sh
cargo build --release      # → target/release/rwave
```

WLF 和 FSDB 后端由默认开启的 `wlf`、`fsdb` 两个 feature 控制，而且编译期只对
`x86_64` Linux 生效；换到别的平台，它们会被编译掉，只剩 VCD、FST、GHW 核心。加上
`--no-default-features` 就能在任意平台上只保留这个核心。解析前端 `wellen` 和它的
FST reader 都放在 `vendor/` 里，所以构建不用联网，用的始终是锁定的那个解析器
版本。

要一次产出四个 release 二进制，用 `scripts/build-release.sh`。它借助
[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild)（用 Zig 作为交叉
链接器）来交叉编译，所以同一套流程在任何机器上都能运行，只有 macOS 目标必须在
macOS 机器上编译。每个目标会自动配好对应的 feature，`linux-amd64` 锁定 glibc
2.17 基线。

| 目标 | Triple | 输出 |
|:--|:--|:--|
| `linux-amd64`   | `x86_64-unknown-linux-gnu`   | `dist/rwave-linux-amd64`       |
| `linux-arm64`   | `aarch64-unknown-linux-musl` | `dist/rwave-linux-arm64`       |
| `windows-amd64` | `x86_64-pc-windows-gnu`      | `dist/rwave-windows-amd64.exe` |
| `macos-arm64`   | `aarch64-apple-darwin`       | `dist/rwave-macos-arm64`       |

```sh
# 一次性准备（macOS）
brew install rustup zig
rustup default stable
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-musl \
                  x86_64-pc-windows-gnu aarch64-apple-darwin

./scripts/build-release.sh                        # 全部四个目标
./scripts/build-release.sh --target linux-amd64   # 只编译一个目标
```

脚本会先检查这些前提，缺少什么就直接打印出对应的安装命令。交叉编译的配置、各
目标的链接方式、完整的 Linux 构建步骤，都写在 [docs/BUILD.md](docs/BUILD.md) 里。

## 命令

```
rwave [--json] [--limit N] [--verbose] <command> <file> [options]
rwave --batch [--json] <file> [global-opts] < commands.txt
```

| 命令 | 作用 |
|:--|:--|
| `info`     | 时间刻度、信号和类型的数量、时间跨度、各个 scope，一眼看清文件 |
| `list`     | 列出信号的路径、位宽、类型（每个别名路径一行） |
| `dump`     | 按时间顺序打印某个时间窗内的每一次值变化 |
| `summary`  | 逐个信号的统计：有没有活动、变化了多少次、上升和下降沿数量、有没有出现过未知位 |
| `snapshot` | 某个时刻（`--at T`）所有已知的信号值 |
| `compare`  | 两个时刻（`--at T1,T2`）之间有哪些变化 |
| `search`   | 找出条件成立的时间区间；配合 `changed(SIG)` 则找出成立的具体时刻 |
| `tree`     | 浏览层级：某个 scope 的子节点，或 `--of SIGNAL` 的完整上级链 |
| `trace`    | *(实验性，默认关闭)* 谁驱动了某个信号、它又驱动了谁，附带 `file:line`。只支持通过内置 Verdi NPI 后端打开的 FSDB；需要设 `RWAVE_TRACE_EN=1` |

- **选择信号。** 除了 `info`、`tree`、`trace`，每条命令都支持四个
  [信号选择](#选择信号)选项；`tree` 只支持 `--scope` 和 `--depth`。
- **时间。** `dump`、`summary`、`search` 用 `--begin`/`--end` 指定时间窗；
  `snapshot`、`compare` 用 `--at` 指定时刻。时间带单位后缀 `fs`、`ps`、`ns`、
  `us`、`ms`、`s`（比如 `17.5us`）；只写数字则按原始 tick 计算。
- **全局选项。** `--json` 输出结构化数据，`--limit N` 限制行数（默认 500，写 `0`
  表示不限），`--verbose` 输出更多字段。结果被截断时，最后一行会说明，`--json`
  下还会带一个 `hint` 字段。
- **条件**（`search` 用）是一串用逗号连起来的 AND 条件，每项是 `SIG=VAL`、
  `SIG!=VAL` 或 `changed(SIG)`；值可以写十进制、十六进制（`0xff`）、二进制
  （`b1010`）或 4 态。写 `changed(SIG)` 会切换到事件模式，报告 SIG 翻转、同时子句
  其余部分也成立的那些时刻。重复写 `--condition`，就是把这些子句用 OR 连接（它们
  要么都包含 `changed()`，要么都不包含）；字符串内部不支持 OR。

完整用法见 `rwave <command> --help`。

## 选择信号

一份 dump 里的信号，往往比你某一个问题需要看的多得多。用下面四个选项来缩小
范围，它们会对每条信号路径依次生效，像一条流水线：

| | |
|:--|:--|
| `--scope P1,P2`  | 在哪棵子树里查找 |
| `--depth N`      | 从这棵子树的根往下走多深（要配合 `--scope`） |
| `--filter K1,K2` | 保留哪些名字 |
| `--exclude K1,K2`| 去掉哪些，最后生效 |

```sh
# 只要这个模块，不包含子模块。
rwave list sim.fst --scope u_tx.u_fifo --depth 1

# 一个状态位，但不包含以它命名的 CDC 同步器。
rwave summary sim.fst --filter tx_fifo_push_err

# 除时钟树以外的一切。
rwave dump sim.fst --begin 1us --end 2us --exclude 'clk,*_clkgen.*'
```

**按名字还是按路径。** 一个 pattern 如果不包含分隔符，就只匹配信号名本身（路径
的最后一段）；如果包含 `.` 或 `/`，就匹配它的完整路径。这个区别很关键：RTL 习惯
用信号名给 CDC 同步器起名（`u_sync_<sig>`），所以用 `tx_fifo_push_err` 匹配路径，
会把这个同步器内部的每条线也一起带出来。想要信号本身，就匹配名字；想定位到某一
段层级，就加上一个点（`--filter 'u_dma.'`）。pattern 里没有 `*` 或 `?` 时按子串
匹配，有的话就是锚定的 glob；`[` 和 `]` 当作普通字符，匹配不区分大小写，用逗号
隔开的多个 pattern 之间是 OR 关系。

**`--scope`** 是逐段匹配的（`u_fifo` 绝不会命中 `u_fifo_ctrl`），选中一棵子树，
连同它下面的所有内容；路径则按段对齐、从后往前匹配。**`--depth`** 从选中的
scope 开始往下计层（直接在里面的信号算作第 1 层），要配合 `--scope` 使用。只有
`tree` 例外，它计的是 scope 而不是信号，从根开始往下走，可以单独用 `--depth`。

**判断是按路径做的，不是按信号。** 一条信号只要有任意一条路径通过了全部选项，
它就会被保留。这就是为什么 `--exclude` 用在“同时又接进某个同步器的线”上是安全
的：外面那条路径把信号保留下来，同步器自己的线（没有外面的路径）被排除掉。
`search` 没有行级过滤，它的 `--condition` 和 `--show` 里的名字本身就是选择，所以
这些选项只是帮忙确定一个名字到底指哪条信号；直接写全路径的话，就完全绕开了选
择。匹配不到不算错误，只是空结果；把值留空（`--filter ''`）等于没写这个选项，
`--batch` 里某一行就是靠这招去掉一个默认值的。

## JSON 输出

加上 `--json`，每条命令都输出紧凑的结构化 JSON。每个时间都给两种形式：原始
tick 数（`*_ticks` 字段）和人能直接读的形式（`*_h` 字段），这样脚本、CI、AI
agent 和终端前的人都能用。信号值也是同样的考虑，尽量紧凑：1 位逻辑信号确定时
是 `0`/`1`，多位总线确定时是去掉前导零的 `0x<hex>`（比如 `0x4`），只要有未知位就
一律是全宽的 `b<bits>`（`bx`、`bz`、`b01x0`），实数和字符串原样给出。判断"有没有
未知位"只看 `b` 前缀，不要找字母 `x`，每个 `0x` 值里都有它。位宽记在每条信号的
元数据里，所以不用再拿十六进制补零来表示。

```sh
rwave --json info sim.fst
rwave --json search sim.fst --condition "state=5" --show data
```

## 批处理模式

大的 FSDB 和 WLF 数据库要通过厂商库来读取，每“打开”一次都要启动一个 C++ 运行
时、把整个层级建立一遍索引，几个 GB 的文件就得耗时几秒到几十秒。如果多次查询都
是针对同一个文件（CI 检查、脚本批量提取、AI agent 的多步流程），那这笔开销只承担
一次、而不是每次查询都承担，就很值。`--batch` 干的就是这件事：文件只加载一次，
然后依次执行从 stdin 读进来的一串命令。

```sh
printf '%s\n' \
  'info' \
  'list --filter clk,state' \
  'dump --begin 1us --end 2us --filter state' \
  'search --condition valid=1,ready=1 --show data' \
  | rwave --batch --json sim.fst
```

每行输入就是一条普通命令，只是开头的 `rwave` 和文件名都省略了，因为这两样在
`--batch` 那次调用里已经定死。空行、以 `#` 开头的行会被跳过；行尾写 `#label`
可以给这一行的结果起个名字。`--batch` 命令行上给的 `[global-opts]`（`--limit`、
`--verbose`、各种选择选项等）会作为默认值，某一行可以覆盖它。在整个 `--batch`
上加一个 `--exclude`，是把某类噪声从每条查询里都排除掉的好办法；每个选项各自独立
覆盖自己的默认值，所以某一行想去掉其中一个，把它传成空值（`--filter ''`）就行，
不影响别的。

结果按输入顺序返回，一条命令对应一条。`--json` 下每条是一个 NDJSON 对象；不加
`--json` 时，每条是一个 `#label` 开头，后面跟着这条命令平常的文本输出：

```
{"id":"1","ok":true,"result":{ …info… }}
{"id":"2","ok":true,"result":{ …list… }}
```

批处理里一条命令的 `result`，和你单独运行这条命令的结果一模一样；批处理只是省去
了重复加载，不会改变任何命令的输出。某条命令失败了（信号名不认识、时间不合
法），会以 `"ok":false` 报告，但不会中断批处理，整轮照样以 `0` 退出。只有文件加
载不了、或者命令流无法读取，才算致命错误。

## WLF 和 FSDB 实验性支持

在 linux-amd64 上，RWaveAnalyzer 实验性支持两种厂商波形数据库：Mentor/Siemens
的 **WLF** 和 Synopsys 的 **FSDB**。它在运行时直接调用各家自己的 reader 库来读
取，不用先转格式，也不产生中间文件。

### WLF

rwave 靠 `libwlf.so` 来读取 Questa / ModelSim 的 `.wlf` 文件。把 `RWAVE_WLF_LIB`
设成你 Questa 安装里这个库的路径：

```sh
export RWAVE_WLF_LIB=/path/to/questa/linux_x86_64/libwlf.so
rwave info run.wlf
```

厂商工具必须安装在同一台机器上；rwave 运行时才加载 `libwlf.so`，自己并不附带这
个库。

### FSDB

rwave 有两种读取 `.fsdb` 的方式，都是实验性的，也都只支持 linux-amd64。

**内置后端（NPI）**，随 `rwave-linux-amd64` 一起发布，不用额外编译。rwave 通过你
Verdi 安装里的 `libNPI.so` 来调用 Synopsys 的 NPI（Novas Programming
Interface）。这条路径需要机器上有 Verdi-Ultra 的 license。用起来只要一个
`VERDI_HOME`：

```sh
export VERDI_HOME=/path/to/verdi     # source Verdi 环境时通常已经设好了
rwave info sim.fsdb
```

rwave 会自己在 `$VERDI_HOME` 下找到 `libNPI.so`，不需要设置 `LD_LIBRARY_PATH`。
只有当安装目录结构不标准时，才需要用 `RWAVE_FSDB_LIB` 指定 `libNPI.so` 的具体
路径。

**插件后端
（[rwave-open-fsdb-plugin](https://github.com/neveltyc/rwave-open-fsdb-plugin)）**
是一个纯源码插件，通过 Synopsys 的 FsdbReader 接口读取 FSDB。你需要在一台装了已
授权 Verdi 的机器上自己编译它，因为编译时会链接到不能再分发的厂商库。这条路径不
需要 NPI 后端要求的那个 Verdi-Ultra license。如果你想在任意 linux-amd64 环境上读
取 FSDB，就选它：

```sh
# 在有 Verdi 的机器上构建
git clone https://github.com/neveltyc/rwave-open-fsdb-plugin
cd rwave-open-fsdb-plugin
./configure && make bundle

# 部署：解开 bundle，把 rwave 指向这个插件
mkdir -p ~/.rwave
tar xzf dist/rwave_fsdb_backend-*-linux_x86_64.tar.gz -C ~/.rwave --strip-components=1
export RWAVE_PLUGIN_FSDB="$HOME/.rwave/librwave_fsdb_backend.so"
rwave info sim.fsdb
```

只要设了 `RWAVE_PLUGIN_FSDB`，读取 `.fsdb` 时它就会覆盖内置的 NPI 后端。

这个插件是按 rwave 的后端 ABI 编译的，当前是 **v2**。rwave 要求版本完全一致，
不一致就会报错并把两个版本号都列出来；遇到这种情况重新编译插件即可。

### 追踪驱动和负载（实验性，默认关闭）

`trace` 回答两个问题：谁驱动了这条信号、谁又在读取它，并给出驱动语句的源码和
`file:line`。它是唯一一条不只读波形的命令，所以要显式打开：没设 `RWAVE_TRACE_EN`
时它会拒绝执行并给出说明。

```bash
export RWAVE_TRACE_EN=1
rwave trace sim.fsdb tb.dut.u_core.u_alu.res
rwave trace sim.fsdb tb.dut.u_core.u_alu.res --load --at 1250ns
```

连接关系来自 Verdi 详细展开（elaborate）后的设计数据库，用
`vcs -kdb -debug_access+all`，或者 `vericom -kdb` 加 `elabcom -elab kdb` 生成。
一般你不用告诉 rwave 它在哪，因为 VCS 会把路径写进 FSDB 文件头。只有当 rwave 说
这个路径访问不到时（比如 dump 被挪出了它原来的构建目录），才需要传入
`--kdb <simv.daidir>`。

`trace` 要求 `.fsdb` 是用内置的 Verdi NPI 后端打开的，所以一旦设了
`RWAVE_PLUGIN_FSDB`，`trace` 就用不了。其他格式则会提示不支持。

| 选项 | 作用 |
|:--|:--|
| `--load` | 查看谁在读取这条信号，而不是谁驱动它 |
| `--at T` | 为每个端点标注它在 T 时刻的值 |
| `--control` | 把时钟、复位，以及外层的 `if`/`case` 依赖也一并纳入 |
| `--kdb DIR` | 设计数据库的路径，用于记录的路径访问不到时 |
| `--top NAME` | 设计的顶层模块名，用于它和波形里的不一致时 |

### 环境变量

| 变量 | 是否必需 | 作用 |
|:--|:--|:--|
| `VERDI_HOME`       | 读 `.fsdb` 时必需 | 你的 Verdi 安装目录。rwave 会在它下面自动找到 `libNPI.so`（以及 `trace` 用的 `libnpiL1.so`）。需要 Verdi-Ultra license。 |
| `RWAVE_WLF_LIB`    | 读 `.wlf` 时必需 | `libwlf.so` 的绝对路径。 |
| `RWAVE_TRACE_EN`   | 用 `trace` 时必需 | 设成 `1` 打开实验性的 `trace` 命令，默认关闭。 |
| `RWAVE_FSDB_LIB`   | 可选 | `libNPI.so` 的绝对路径。用来覆盖 `VERDI_HOME` 的自动查找，供目录结构不标准时使用。 |
| `RWAVE_NPI_L1_LIB` | 可选 | `libnpiL1.so`（NPI 连接库，`trace` 用）的绝对路径。当它不在 `libNPI.so` 旁边时用来指定。 |
| `RWAVE_PLUGIN_FSDB` | 可选 | 插件编译出的 `librwave_fsdb_backend.so` 的绝对路径，会覆盖内置的 FSDB 后端。 |

对于其他格式，或者你自己实现的后端，rwave 会从 `$RWAVE_PLUGIN_<EXT>` 加载任何实
现了它 C ABI 的共享库，详见 [docs/PLUGIN.md](docs/PLUGIN.md)。

## 免责声明

RWaveAnalyzer 读取 WLF 和 FSDB 时，只经过各厂商自己的 reader 库接口。它不包含任
何厂商的二进制或源码，编译时不链接它们，也不分发任何厂商软件；运行时加载的
reader 库，是你从自己已授权的安装里提供的。
[rwave-open-fsdb-plugin](https://github.com/neveltyc/rwave-open-fsdb-plugin) 同样
是纯源码，不带任何厂商二进制，你用自己的 Verdi 安装来编译它。读取这些格式需要厂
商的软件，需要的时候还得你机器上有有效的 license；按厂商条款获取和使用它们，是你
自己的责任。

## 面向 AI agent

仓库里带了一个 agent skill，在 [skill/SKILL.md](skill/SKILL.md)：包含一棵从用户
意图到命令的决策树、一份 JSON 字段速查、条件语法、WLF/FSDB 的配置，还有几个调试
流程。把你的 agent 指向这个文件，剩下的就交给每条命令的 `--json` 输出。

## 架构

crate 自上而下分层，每一层只依赖它下面的层：

```
        cli            只做参数解析
         │
      commands         每条命令的逻辑和呈现（文本 / JSON）
         │
       model           与格式无关的领域层：信号表、replay、快照
         │
      backend          WaveformBackend trait（解析器契约）
         │
  wellen_backend       唯一接触 wellen 解析器的代码
```

最关键的边界是 **`WaveformBackend`** 这个 trait。后端把每条信号完整解码好、各自
持有的 trace（时间数组和值数组并排放）交给 model；replay、归并、快照这些逻辑全
在 model 里，只跟切片打交道。因为这个 trait 的粒度很粗，没有逐个采样的虚调用，
热路径就保持单态，所以增加一个解析器，只是往 `backend/` 下添加一个文件。厂商格式
和插件格式也从同一条边界进来：后端既可以是编进二进制的 vtable（`plugin/builtin/`），
也可以是 `dlopen` 进来的库（`plugin/loader.rs`），两者都通过 `plugin_backend.rs`
以及 [`crates/rwave/include/rwave_backend.h`](crates/rwave/include/rwave_backend.h)
里的 C ABI 来驱动。

仓库顶层是这样组织的：

```
crates/rwave/      rwave crate（CLI、model、后端、插件 ABI）
vendor/            vendor 进来的解析前端：wellen 加一个打过补丁的 fst-reader
verify/            自测 harness，激励也提交进了仓库
scripts/           release 构建和激励生成脚本
skill/             agent-skill 描述文件
docs/              扩展文档（BUILD、PLUGIN）
.github/workflows/ CI（ci.yml）、release（release.yml）、benchmark（bench.yml）
```

## 性能

- **Replay** 是在所选信号的 trace 上做二叉最小堆的 k 路归并，k 条信号上共 n 次变
  化时是 `O(n log k)`；同一时间戳内谁先谁后，由写入（声明）顺序决定。
- **快照和 `compare`** 对每条信号做二分查找，定位目标时间点（或之前）的最后一个
  值，不用完整 replay。
- **处理整个文件的命令**（`summary`，以及不带过滤的
  `dump`/`snapshot`/`compare`）把信号分批解码，每批内存有上限，处理完一批就释放
  一批，所以峰值内存只和一批成正比，而不是整个文件。`summary` 在一个几乎不分配
  内存的循环里，直接从每条 trace 计算出它的统计；`dump` 只在一个有上限的堆里保留
  最早的 `--limit` 个事件。

这些流式路径的输出，和简单的一次性（eager）路径逐字节一致；用哪条只是根据所选信
号数量所做的内存和吞吐优化，不影响结果。

## 测试

```sh
cargo test                  # 单元测试：格式化、过滤、条件、CLI
bash verify/run.sh          # 冒烟测试，外加在内置激励上比对 VCD/FST 是否一致
```

`verify/run.sh` 只需要编译好的二进制：它验证每条命令在 VCD 和 FST 上都能运行，而
且那些会给出具体值的命令，对同一个设计在两种格式上结果一致。这是一张自包含的回
归网，不依赖任何外部参照。

## 许可证

MIT，见 [LICENSE](LICENSE)，覆盖 rwave 自己的代码。vendor 进来的组件各自保留自己
的许可证：`vendor/wellen` 和 `vendor/fst-reader` 都是 BSD-3-Clause，各自的许可证
文件保留在它们自己的目录里。

因为 rwave 是以单个静态链接的二进制发布的，下载到的 release 文件周围没有仓库，
所以它的依赖在二进制再分发时要求的那些声明，都汇总在
[third-party-licenses.md](third-party-licenses.md) 里，随每次 release 一起发布。
依赖变动之后，重新生成它：

```sh
bash scripts/gen-licenses.sh
```
