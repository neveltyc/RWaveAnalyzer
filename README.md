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

不用打开 GUI，在终端里一条命令就能查到 `arvalid` 和 `arready` 何时同时为高：

```sh
rwave search sim.fst --condition "arvalid=1,arready=1" --show araddr,arlen
```

- **单个二进制**，没有运行时依赖。
- **多种格式**：VCD、FST、GHW；linux-amd64 上实验性支持 WLF 和 FSDB。
- **JSON 输出**：每条命令都支持 `--json`，键名固定，方便脚本、CI 和 AI agent 读取。
- **大文件友好**：处理整个文件的命令按批流式进行，内存有上限。

## 快速上手

```sh
# 这个文件里有什么？
rwave info sim.fst

# 看看时钟和复位
rwave list sim.fst --filter '*clk*,*rst*'

# 100 ns 到 200 ns 之间发生了什么？
rwave dump sim.fst --begin 100ns --end 200ns --filter state

# valid 和 ready 什么时候同时为高？
rwave search sim.fst --condition "valid=1,ready=1" --show data

# req 在 ready 为低时翻转的那些时刻？
rwave search sim.fst --condition "changed(req),ready=0" --show state

# 任意一个通道握手的时刻？（重复 --condition 就是 OR）
rwave search sim.fst --condition "ch0_valid=1,ch0_ready=1" --condition "ch1_valid=1,ch1_ready=1"

# 17.55 us 时的值
rwave snapshot sim.fst --at 17.55us --filter state,init_done

# 两个时刻之间有哪些变化？
rwave compare sim.fst --at 17.5us,17.7us --filter bus

# 哪些信号有活动，哪些是静态的？
rwave summary sim.fst --filter alu

# 只看这个模块，不包含子模块
rwave list sim.fst --scope u_tx.u_fifo --depth 1

# 除了 CDC 同步器以外的一切
rwave summary sim.fst --exclude '*_sync_*.*'
```

## 安装

从[最新 release](https://github.com/neveltyc/RWaveAnalyzer/releases/latest) 下载对应平台的二进制：

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

`rwave-linux-amd64` 需要 glibc 2.17 及以上。

## 从源码构建

需要 Rust 1.90 及以上的稳定版工具链，不需要其他系统依赖。

```sh
cargo build --release                         # → target/release/rwave
cargo build --release --no-default-features   # 只保留 VCD/FST/GHW
```

WLF、FSDB 后端只在 x86_64 Linux 上编译，其他平台的产物只有 VCD/FST/GHW。

交叉编译 release 二进制要用 [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild)，macOS 目标只能在 macOS 上编译：

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

更多细节见 [docs/BUILD.md](docs/BUILD.md)。测试：

```sh
cargo test
bash verify/run.sh
```

## 命令

```
rwave [--json] [--limit N] [--verbose] [-o FILE] <command> <file> [options]
rwave --batch [--json] <file> [global-opts] < commands.txt
```

| 命令 | 作用 |
|:--|:--|
| `info`     | 时间刻度、信号数量、时间跨度、scope 概览 |
| `list`     | 列出信号的路径、位宽、类型 |
| `dump`     | 时间窗内的每一次值变化 |
| `summary`  | 每条信号的变化次数、上升和下降沿数、是否出现未知位 |
| `snapshot` | 某个时刻所有信号的值 |
| `compare`  | 两个时刻之间有哪些变化 |
| `search`   | 条件成立的时间区间，或 `changed()` 触发的时刻 |
| `tree`     | 浏览层级：某个 scope 的子节点，或 `--of SIGNAL` 的上级链 |
| `trace`    | 信号的驱动和负载，附带 `file:line`（仅限 FSDB，见 [trace](#追踪驱动和负载)） |

| 选项 | 作用 |
|:--|:--|
| `--json`            | 输出紧凑 JSON，见 [JSON 输出](#json-输出) |
| `--limit N`         | 最多输出 N 行，默认 500（用了 `-o` 或 `--verbose` 时不限），`0` 表示不限；被截断时最后一行会提示 |
| `--verbose`         | 输出更多字段 |
| `-o, --output FILE` | 完整结果写进 FILE（成功时才生成），stdout 只打印一行回执 |
| `--begin T` / `--end T` | 时间窗，用于 `dump`、`summary`、`search` |
| `--at T`            | 时刻，用于 `snapshot`；`compare` 写成 `--at T1,T2` |

- 时间带单位后缀 `fs`、`ps`、`ns`、`us`、`ms`、`s`（如 `17.5us`），只写数字表示原始 tick。
- 输出到终端时超过 1 MiB 会截断，退出码为 3；写到管道或文件不受限制。
- `rwave --help` 是语法速查，`rwave --help --verbose` 输出完整手册。

### 搜索条件

| 写法 | 含义 |
|:--|:--|
| `SIG=VAL` / `SIG!=VAL` | 等于 / 不等于 |
| `changed(SIG)`         | SIG 翻转的时刻，结果是时刻而不是区间 |
| `--show K1,K2`         | 区间按显示信号的取值变化切段，行数取决于 `--show` |
| `A=1,B=0`              | 逗号连接，AND |
| 重复 `--condition`     | 各组之间 OR；要么都含 `changed()`，要么都不含 |

| 值 | 写法 |
|:--|:--|
| 十进制   | `cnt=5`；负数按位宽取补码，`cnt=-1` |
| 十六进制 | `addr=0xff` |
| 二进制 / 4 态 | `state=b1010`、`bus=bx`；`?` 是无关位，`status=b?????1??` 只看第 2 位（位数不足时高位补 0，所以要写满位宽） |
| real     | 按数值比较，`temp=-1.5` |
| string   | 数字或位字面量按文本比较，`s=5` |

信号不可能取到的值会直接报错，比如 4 位的 `state=99`。

## 选择信号

除了 `info`、`tree`、`trace`，每条命令都支持下面五个选项（`tree` 只支持 `--scope` 和 `--depth`）：

| 选项 | 作用 |
|:--|:--|
| `--scope P1,P2`   | 只看这些子树；按段匹配，`u_fifo` 不会命中 `u_fifo_ctrl` |
| `--depth N`       | 从 scope 往下几层，直属信号算第 1 层；要配合 `--scope`（`tree` 除外） |
| `--filter K1,K2`  | 保留匹配的信号 |
| `--exclude K1,K2` | 去掉匹配的信号，最后生效 |
| `--exact`         | 只按完整名字匹配，不回退到子串匹配 |

匹配规则：

- 不含 `.` 或 `/` 的 pattern 匹配信号名，含有则匹配完整路径，比如 `--filter 'u_dma.'`；两种分隔符通用。
- 有信号的名字与 pattern 完全一致时只选它：`top.sig3` 不会选中 `top.sig30`。路径可以从任意一段写起（`u_dma.req`），总线范围可以省略（`state` 选中 `state[2:0]`）。
- 没有完全一致的名字时按子串匹配；前缀匹配写 `name*`。
- 带 `*`、`?` 的是锚定 glob，总线范围可以省略（`*data` 能匹配 `data[7:0]`），`[`、`]` 按字面匹配，不区分大小写，逗号分隔表示 OR。
- `--exclude` 以及 `search` 的 `--condition`、`--show` 用同一套规则：`--exclude clk` 只去掉 `clk`，要去掉所有时钟写 `--exclude '*clk*'`。
- 没有匹配不算错误，只返回空结果；`--filter ''` 等于没写。

## JSON 输出

```sh
rwave --json info sim.fst
rwave --json search sim.fst --condition "state=5" --show data
```

- 时间同时给出原始 tick（`*_ticks`）和可读形式（`*_h`）。
- 同一条命令、同一组 flag，键集和顺序都固定，没内容时写 `null`、`0`、`[]` 或 `{}`。
- 出错时把 JSON 写到 stderr：`{"command":"dump","ok":false,"error":"..."}`。

| 值类型 | 写法 |
|:--|:--|
| 1 位逻辑 | `0`、`1` |
| 多位总线 | `0x<hex>`，不补零，如 `0x4` |
| 含未知位 | 全宽 `b<bits>`，如 `bx`、`b01x0`；判断有没有未知位要看 `b` 前缀，不要找 `x` |
| real     | 能精确还原的最短十进制，如 `1.5`、`NaN` |
| string   | 原样 |

| 退出码 | 含义 |
|:--|:--|
| 0 | 给出了结果，空结果也算（`hint` 会说明原因） |
| 1 | 文件或环境问题：文件打不开或解析失败、设计库或 license 不可用、结果写不出去 |
| 2 | 请求有误：参数、信号名、条件或时间写错，改命令即可 |
| 3 | 终端输出超过 1 MiB 被截断；改为写到文件或管道 |

每条命令的字段见 [docs/JSON.md](docs/JSON.md)。

## 批处理模式

`--batch` 只加载一次文件，然后依次执行 stdin 里的命令。对同一个大文件（尤其是 FSDB、WLF）做多次查询时能省下大量加载时间。

```sh
printf '%s\n' \
  'info' \
  'list --filter clk,state' \
  'dump --begin 1us --end 2us --filter state' \
  'search --condition valid=1,ready=1 --show data' \
  | rwave --batch --json sim.fst
```

```
{"id":"1","ok":true,"result":{ …info… }}
{"id":"2","ok":true,"result":{ …list… }}
```

- 每行一条命令，省略开头的 `rwave` 和文件名。空行和 `#` 开头的行会跳过，行尾写 `#label` 可以给结果命名。
- 命令行上的选项是每行的默认值，单行可以覆盖；传空值（`--filter ''`）可以去掉某个默认值。
- 结果按输入顺序输出，和单独运行完全一致。`--json` 下每条一行 NDJSON，文本模式下每条以 `#label` 行开头。
- 单条命令失败时报告 `"ok":false` 和它单独运行时的 `exit`，不影响后续命令。

## WLF 与 FSDB（实验性）

只支持 linux-amd64。rwave 运行时调用厂商自己的库直接读取，不需要转格式。

### WLF

```sh
export RWAVE_WLF_LIB=/path/to/questa/linux_x86_64/libwlf.so
rwave info run.wlf
```

### FSDB

有两种方式：

| | 内置 NPI 后端 | [rwave-open-fsdb-plugin](https://github.com/neveltyc/rwave-open-fsdb-plugin) |
|:--|:--|:--|
| 获取 | 已包含在 `rwave-linux-amd64` 里 | 在装有 Verdi 的机器上自行编译 |
| license | 需要 Verdi-Ultra | 不需要 Verdi-Ultra |
| 设置 | `VERDI_HOME` | `RWAVE_PLUGIN_FSDB` |
| `trace` | ✓ | — |

内置后端：

```sh
export VERDI_HOME=/path/to/verdi     # source Verdi 环境后通常已经设好
rwave info sim.fsdb
```

插件：

```sh
# 在有 Verdi 的机器上编译
git clone https://github.com/neveltyc/rwave-open-fsdb-plugin
cd rwave-open-fsdb-plugin
./configure && make bundle

# 部署
mkdir -p ~/.rwave
tar xzf dist/rwave_fsdb_backend-*-linux_x86_64.tar.gz -C ~/.rwave --strip-components=1
export RWAVE_PLUGIN_FSDB="$HOME/.rwave/librwave_fsdb_backend.so"
rwave info sim.fsdb
```

设置了 `RWAVE_PLUGIN_FSDB` 时，插件优先于内置后端。插件和 rwave 的后端 ABI 版本（当前为 v2）必须一致，不一致时会报错，重新编译插件即可。

### 追踪驱动和负载

`trace` 查找信号的驱动或负载，并给出对应的源码语句和 `file:line`。只支持内置 NPI 后端打开的 FSDB，运行时会占用一个 license。

```bash
rwave trace sim.fsdb tb.dut.u_core.u_alu.res
rwave trace sim.fsdb tb.dut.u_core.u_alu.res --load --at 1250ns
```

需要 Verdi 设计库，用 `vcs -kdb -debug_access+all`，或 `vericom -kdb` 加 `elabcom -elab kdb` 生成。rwave 会从 FSDB 文件头读出设计库路径，路径失效时用 `--kdb` 指定。

| 选项 | 作用 |
|:--|:--|
| `--load`     | 查看谁在读这条信号，而不是谁驱动它 |
| `--at T`     | 标出每个端点在 T 时刻的值 |
| `--control`  | 把时钟、复位和外层 `if`/`case` 依赖也纳入 |
| `--kdb DIR`  | 设计库路径 |
| `--top NAME` | 设计顶层模块名，和波形里的不一致时使用 |

### 环境变量

| 变量 | 是否必需 | 作用 |
|:--|:--|:--|
| `VERDI_HOME`        | 读 `.fsdb` 时必需 | Verdi 安装目录，rwave 会在其中查找 `libNPI.so` 和 `libnpiL1.so` |
| `RWAVE_WLF_LIB`     | 读 `.wlf` 时必需 | `libwlf.so` 的路径 |
| `RWAVE_TRACE_EN`    | 可选 | 设为 `0` 禁用 `trace` |
| `RWAVE_FSDB_LIB`    | 可选 | `libNPI.so` 的路径，Verdi 目录结构不标准时使用 |
| `RWAVE_NPI_L1_LIB`  | 可选 | `libnpiL1.so` 的路径，它不在 `libNPI.so` 旁边时使用 |
| `RWAVE_PLUGIN_FSDB` | 可选 | FSDB 插件 `librwave_fsdb_backend.so` 的路径 |

其他格式可以通过 `$RWAVE_PLUGIN_<EXT>` 加载自己实现的后端，见 [docs/PLUGIN.md](docs/PLUGIN.md)。

## 免责声明

rwave 不包含、不链接、不分发任何厂商软件，运行时加载的是你自己已授权安装中的库。按厂商条款获取和使用这些软件及 license，由你自己负责。

## 面向 AI agent

[skill/SKILL.md](skill/SKILL.md) 是给 agent 用的 skill，也是 `rwave --help --verbose` 输出的完整手册，包含使用原则、命令选择、JSON 字段速查和常见调试流程。

## 许可证

MIT，见 [LICENSE](LICENSE)。`vendor/wellen` 和 `vendor/fst-reader` 采用 BSD-3-Clause。release 二进制中依赖的许可证声明见 [third-party-licenses.md](third-party-licenses.md)。
